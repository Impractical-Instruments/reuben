#!/usr/bin/env bash
# Perf regression gate (ADR-0019).
#
# Compares instruction counts (callgrind `Ir`) between the PR's library code and a baseline
# ref, using the PR's *own* bench harness for both sides — so a bench that doesn't exist yet
# on the baseline commit still works (we never swap the benches). Deterministic instruction
# counts mean no wall-clock flake on the shared runner.
#
# TWO LAYERS (ADR-0019, #30), each gated independently so one can't mask the other:
#   - macro_iai: end-to-end `render_block` per instrument.
#   - micro_iai: per-operator `process` (needs the `bench` feature's crate-private bridge).
# Each layer runs its own baseline/compare cycle. If a layer's harness postdates the baseline
# (e.g. the PR that introduces micro_iai — the baseline `src/` has no `bench_support`), that
# layer's baseline build fails and it is skipped with a note, while the other still gates.
#
# Within the micro layer we also skip PER OPERATOR: an operator the PR added or renamed isn't in the
# baseline registry, so benching it against the baseline `src/` would panic and abort the whole layer.
# We compute those baseline-absent kinds (HEAD's micro census minus the baseline's) and pass them in
# REUBEN_MICRO_BENCH_SKIP to BOTH runs, so each new operator is dropped from the comparison
# symmetrically while every operator that existed at the base is still benched and gated.
#
# What we DO swap to the baseline ref: the engine source of EVERY crate in reuben-core's build
# closure (reuben-core + reuben-macros + reuben-contract `src/`) AND the instrument fixtures
# (`instruments/`), together. Swapping reuben-core/src alone is not enough: its operators call
# `Self::contract()`, emitted by the reuben-macros proc-macro — so a PR that changes that macro
# (as ADR-0030 did) leaves baseline reuben-core/src compiled against HEAD's macro, which no longer
# emits `contract`, and the baseline build fails. Moving the whole source closure together keeps
# the snapshot self-consistent. Bench harnesses (`benches/`) are never swapped; `bench_support`
# lives in reuben-core/src, so a baseline that predates it still skips the micro layer as intended.
# The harness embeds those JSONs via `include_str!`, and the JSON is a *wire
# format versioned with the engine* — a format-migration PR (e.g. ADR-0028 moved op params
# to `Float`/`Enum` inputs) makes the new JSON unloadable by old `src/`. Swapping only
# `src/` would then bench old code against new-format JSON, which mis-loads into a cheap
# degenerate graph and produces a bogus, far-too-low baseline (a false regression of
# several hundred %). Each side must read JSON valid for ITS OWN engine, so we hold the
# *semantic* workload fixed (same instrument, same notes) while letting code AND its data
# representation move together. For non-format PRs the fixtures are byte-identical across
# refs, so this is a no-op and the comparison is unchanged.
#
#   FAIL (exit 1): any benched instrument regresses Ir by > 10%  (enforced by callgrind
#                  itself via --callgrind-limits; the authoritative verdict).
#   WARN:          3%..10% — surfaced in the job summary + as a GH annotation, non-blocking.
#
# Arg 1: baseline commit SHA (empty => no comparison, just run once).
set -uo pipefail

BASE_SHA="${1:-}"
PKG="reuben-core"
# Both iai layers (ADR-0019, #30). macro_iai needs no features; micro_iai needs `bench` for the
# crate-private `Io` bridge. The feature only compiles `bench_support` (dead code for macro_iai),
# so it leaves macro Ir byte-stable — safe to pass on both runs.
BENCHES=("macro_iai" "micro_iai")
FEATURES="bench"
# reuben-core's full source closure — every crate whose `src/` feeds the core build (see header).
# These move to the baseline ref together so the snapshot is self-consistent; reuben-core/src alone
# would leave operator `Self::contract()` calls compiled against HEAD's macro. reuben-native is
# excluded: it is not in `cargo bench -p reuben-core`'s build graph. If a new crate joins the
# closure and is missed here, the baseline library build fails and the hard-fail guard below trips
# (rather than masking it as a skip).
SRC=(
  "crates/${PKG}/src"
  crates/reuben-macros/src
  crates/reuben-contract/src
)
# Version-locked fixtures the harness embeds via `include_str!`. Swapped with `src/` so each
# side reads JSON valid for its own engine (see header). Space-separated list of pathspecs.
FIXTURES="instruments"
SUMMARY="${GITHUB_STEP_SUMMARY:-/dev/stdout}"
FAIL_PCT=10
WARN_PCT=3

note() { printf '%s\n' "$*" >>"$SUMMARY"; }
run_bench() { local bench="$1"; shift; cargo bench -p "$PKG" --features "$FEATURES" --bench "$bench" -- "$@"; }

note "## Perf gate — instruction counts (macro \`render_block\` + per-operator \`process\`)"
note ""

# No usable baseline (new branch's first push, or a null/unknown SHA): run each once so the
# harness is still exercised, but there's nothing to compare against.
if [ -z "$BASE_SHA" ] || ! git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
  note "_No usable baseline ref — ran benches once, no comparison._"
  for b in "${BENCHES[@]}"; do run_bench "$b" || true; done
  exit 0
fi

note "Baseline: \`$(git rev-parse --short "$BASE_SHA")\` · fail > ${FAIL_PCT}% · warn > ${WARN_PCT}%"
note ""

# PR-new operators have no baseline counterpart: the baseline commit's swapped-in src/ doesn't
# register them, so the HEAD micro harness would panic building their driver and abort the WHOLE
# micro layer — the masking bug on #104, where renaming `map` -> `map_f32_signal` (+ adding
# `map_f32_value`) skipped every operator's gate and CI stayed green. Compute those kinds as HEAD's
# micro census minus the baseline's, and hand the list to the bench via REUBEN_MICRO_BENCH_SKIP. The
# harness skips exactly these, symmetrically on BOTH the baseline and PR runs, so a brand-new operator
# is excluded from the comparison (nothing to compare it against) while every operator that existed at
# the base is still benched and gated. `MICRO_IAI_KINDS` mirrors the registry (forcing function #30),
# so it's an exact, build-free census of each side's operators. macro_iai ignores the var.
micro_kinds() { sed -n '/MICRO_IAI_KINDS/,/];/p' | grep -oE '"[a-z0-9_]+"' | tr -d '"' | LC_ALL=C sort -u; }
head_kinds="$(micro_kinds <"crates/${PKG}/src/bench_support.rs")"
base_kinds="$(git show "${BASE_SHA}:crates/${PKG}/src/bench_support.rs" 2>/dev/null | micro_kinds)"
REUBEN_MICRO_BENCH_SKIP="$(comm -23 <(printf '%s\n' "$head_kinds") <(printf '%s\n' "$base_kinds") | paste -sd, -)"
export REUBEN_MICRO_BENCH_SKIP
if [ -n "$REUBEN_MICRO_BENCH_SKIP" ]; then
  note "_New operators since baseline — benched but not gated (no baseline to compare): \`${REUBEN_MICRO_BENCH_SKIP}\`._"
  note ""
fi

overall_fail=0
skipped=0
hard_broken=0

# Gate one bench layer: baseline run (old src + fixtures, PR harness) -> compare PR run -> table.
#
# A baseline run can fail two very different ways, and conflating them is how a broken gate slips
# through green (the original masking bug). We split them by whether the bench target COMPILES:
#   - The HEAD bench HARNESS does not compile against the baseline src  => the harness postdates the
#     baseline API (e.g. the PR that introduced this layer — baseline src has no `bench_support` — or
#     a moved workload bridge). No apples-to-apples baseline exists; skip this layer, non-blocking.
#   - The bench COMPILES but the run fails  => a runtime panic/abort against an otherwise-buildable
#     baseline (a broken source swap, or a genuine breakage). NEVER skip it — fail the gate. We
#     classify on compile, not library-build, because a runtime crash (e.g. an operator missing from
#     the baseline registry) leaves the library perfectly buildable yet must still fail, not skip.
#     PR-new operators are removed from this failure mode upstream: REUBEN_MICRO_BENCH_SKIP excludes
#     them so they never reach the baseline registry lookup.
gate_one() {
  local bench="$1"
  note "### \`$bench\`"
  note ""

  # 1) Baseline engine + its own fixtures (PR bench harness) -> save as baseline "base".
  #    Swap src/ and instruments/ together so the baseline reads JSON it can actually load.
  git checkout "$BASE_SHA" -- "${SRC[@]}" $FIXTURES
  if ! run_bench "$bench" --save-baseline=base; then
    # The baseline bench did not build/run. Classify by COMPILE, not by run. The ONLY legitimate
    # skip is "HEAD's bench harness postdates the baseline API" — a *compile* incompatibility of the
    # bench target against the swapped baseline src (e.g. the PR that introduced this layer: baseline
    # src has no `bench_support`, so HEAD's `use ...::OpHarness` won't compile). Probe exactly that
    # with `--no-run` against the still-checked-out baseline src; its compile errors are already in
    # the log above, so the exit code is all we need.
    if ! cargo bench -p "$PKG" --features "$FEATURES" --bench "$bench" --no-run >/dev/null 2>&1; then
      git checkout HEAD -- "${SRC[@]}" $FIXTURES
      skipped=$((skipped + 1))
      printf '::warning title=Perf layer skipped::%s HEAD bench harness does not compile against the baseline — no comparison for this layer\n' "$bench"
      note "⚠️ \`$bench\`: the HEAD bench harness does not compile against the baseline src — its harness postdates the baseline (workload API or bench bridge moved). Layer skipped, non-blocking."
      note ""
      return 0
    fi
    # Compiles but the run failed: a panic/abort at RUNTIME, not an API gap. New operators no longer
    # crash here (the gate skips them via REUBEN_MICRO_BENCH_SKIP), so a surviving run failure is real
    # — fail the gate. Treating a runtime crash as a skip is exactly the masking bug we are closing.
    git checkout HEAD -- "${SRC[@]}" $FIXTURES
    hard_broken=1
    overall_fail=1
    printf '::error title=Baseline bench broken::%s baseline bench compiled but failed at runtime — perf gate cannot certify no regression\n' "$bench"
    note "❌ \`$bench\`: the bench compiles against the baseline but failed at **runtime** (panic/abort) — not skipping; skipping a runtime failure is how a broken gate slips through green."
    note ""
    return 0
  fi

  # 2) Restore PR engine + fixtures, compare vs "base". callgrind enforces the hard limit and
  #    exits non-zero if any case breaches it; that exit code is this layer's verdict.
  git checkout HEAD -- "${SRC[@]}" $FIXTURES
  run_bench "$bench" --baseline=base --save-summary=json --callgrind-limits="ir=${FAIL_PCT}%"
  local gate_rc=$?

  # Best-effort per-case table from this bench's saved summaries (path-scoped to the bench).
  # diff_pct is serialized as a string (callgrind event "Ir" = instructions). The jq walk is
  # position-independent so it survives schema nesting changes; if it finds nothing the callgrind
  # exit code still rules.
  note "| Case | Ir Δ% | Status |"
  note "|---|---:|:---:|"
  local table_fail=0 parsed=0
  while IFS= read -r f; do
    id=$(jq -r '.id // .function_name // "?"' "$f" 2>/dev/null)
    pct=$(jq -r 'first(.. | objects | select(has("Ir")) | .Ir | objects | select(has("diffs")) | .diffs | objects | .diff_pct) // empty' "$f" 2>/dev/null)
    [ -z "$pct" ] && continue
    parsed=1
    # Classify only finite numbers. A skipped new operator renders an 11-instruction no-op, and iai
    # can serialize its diff vs a degenerate 0 baseline as "inf"/"nan" in a secondary context — the
    # authoritative callgrind compare reports it as "No change", so never let such a value drive
    # `table_fail` (which feeds the gate verdict). Non-numeric ⇒ ok.
    status=$(awk -v p="$pct" -v f="$FAIL_PCT" -v w="$WARN_PCT" 'BEGIN{
      if (p !~ /^[+-]?[0-9]/) { print "ok"; exit }
      if (p+0 >= f) print "FAIL"; else if (p+0 >= w) print "WARN"; else print "ok"}')
    case "$status" in
      FAIL) icon="❌"; table_fail=1; printf '::error title=Perf regression::%s %s Ir +%s%% (>%s%%)\n' "$bench" "$id" "$pct" "$FAIL_PCT" ;;
      WARN) icon="⚠️"; printf '::warning title=Perf creep::%s %s Ir +%s%% (>%s%%)\n' "$bench" "$id" "$pct" "$WARN_PCT" ;;
      *)    icon="✅" ;;
    esac
    note "| ${id} | ${pct} | ${icon} |"
  done < <(find target/iai -path "*${bench}*" -name summary.json 2>/dev/null)

  if [ "$parsed" -eq 0 ]; then
    note "| _(summary parse unavailable — verdict from callgrind exit code)_ |  |  |"
  fi
  note ""

  # This layer's verdict: callgrind's own limit breach OR our table classifying a >FAIL_PCT regress.
  if [ "$gate_rc" -ne 0 ] || [ "$table_fail" -ne 0 ]; then
    note "_\`$bench\`: ❌ regression over ${FAIL_PCT}%._"
    note ""
    overall_fail=1
  fi
}

for b in "${BENCHES[@]}"; do gate_one "$b"; done

# A baseline bench that COMPILED but failed at runtime (vs. a harness that merely postdates the
# baseline API) is fatal — the gate could not certify "no regression," and silently passing it is the
# masking bug we are closing.
if [ "$hard_broken" -ne 0 ]; then
  note "**Result: baseline bench failed to run — perf gate could not run a comparison. Not a pass.**"
  exit 1
fi

if [ "$overall_fail" -ne 0 ]; then
  note "**Result: ❌ regression over ${FAIL_PCT}%.**"
  exit 1
fi
if [ "$skipped" -ne 0 ]; then
  note "**Result: ✅ within ${FAIL_PCT}% — but ${skipped}/${#BENCHES[@]} layer(s) skipped (partial coverage).**"
  exit 0
fi
note "**Result: ✅ within ${FAIL_PCT}%.**"
exit 0
