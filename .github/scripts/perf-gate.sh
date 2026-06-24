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
# What we DO swap to the baseline ref: `src/` AND the instrument fixtures (`instruments/`),
# together. The harness embeds those JSONs via `include_str!`, and the JSON is a *wire
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
SRC="crates/${PKG}/src"
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

overall_fail=0

# Gate one bench layer: baseline run (old src + fixtures, PR harness) -> compare PR run -> table.
# A layer whose harness postdates the baseline (its baseline build fails) is skipped, not fatal.
gate_one() {
  local bench="$1"
  note "### \`$bench\`"
  note ""

  # 1) Baseline engine + its own fixtures (PR bench harness) -> save as baseline "base".
  #    Swap src/ and instruments/ together so the baseline reads JSON it can actually load.
  git checkout "$BASE_SHA" -- "$SRC" $FIXTURES
  if ! run_bench "$bench" --save-baseline=base; then
    git checkout HEAD -- "$SRC" $FIXTURES
    note "⚠️ Baseline did not build/run \`$bench\` — layer skipped (its harness likely postdates the baseline)."
    note ""
    return 0
  fi

  # 2) Restore PR engine + fixtures, compare vs "base". callgrind enforces the hard limit and
  #    exits non-zero if any case breaches it; that exit code is this layer's verdict.
  git checkout HEAD -- "$SRC" $FIXTURES
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
    status=$(awk -v p="$pct" -v f="$FAIL_PCT" -v w="$WARN_PCT" 'BEGIN{
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

if [ "$overall_fail" -ne 0 ]; then
  note "**Result: ❌ regression over ${FAIL_PCT}%.**"
  exit 1
fi
note "**Result: ✅ within ${FAIL_PCT}%.**"
exit 0
