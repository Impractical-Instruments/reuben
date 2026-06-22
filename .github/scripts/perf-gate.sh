#!/usr/bin/env bash
# Perf regression gate (ADR-0019).
#
# Compares end-to-end `render_block` instruction counts (callgrind `Ir`) between the
# PR's library code and a baseline ref, using the PR's *own* bench harness for both
# sides — so a bench that doesn't exist yet on the baseline commit still works (we swap
# only `src/`, never the benches). Deterministic instruction counts mean no wall-clock
# flake on the shared runner.
#
#   FAIL (exit 1): any benched instrument regresses Ir by > 10%  (enforced by callgrind
#                  itself via --callgrind-limits; the authoritative verdict).
#   WARN:          3%..10% — surfaced in the job summary + as a GH annotation, non-blocking.
#
# Arg 1: baseline commit SHA (empty => no comparison, just run once).
set -uo pipefail

BASE_SHA="${1:-}"
PKG="reuben-core"
BENCH="macro_iai"
SRC="crates/${PKG}/src"
SUMMARY="${GITHUB_STEP_SUMMARY:-/dev/stdout}"
FAIL_PCT=10
WARN_PCT=3

note() { printf '%s\n' "$*" >>"$SUMMARY"; }
run_bench() { cargo bench -p "$PKG" --bench "$BENCH" -- "$@"; }

note "## Perf gate — \`render_block\` instruction counts"
note ""

# No usable baseline (new branch's first push, or a null/unknown SHA): run once so the
# harness is still exercised, but there's nothing to compare against.
if [ -z "$BASE_SHA" ] || ! git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
  note "_No usable baseline ref — ran benches once, no comparison._"
  run_bench || true
  exit 0
fi

note "Baseline: \`$(git rev-parse --short "$BASE_SHA")\` · fail > ${FAIL_PCT}% · warn > ${WARN_PCT}%"
note ""

# 1) Baseline library code (+ PR bench harness) -> save as baseline "base".
git checkout "$BASE_SHA" -- "$SRC"
if ! run_bench --save-baseline=base; then
  git checkout HEAD -- "$SRC"
  note "⚠️ Baseline source did not build/run against the current bench harness — gate skipped."
  exit 0
fi

# 2) Restore PR library code, compare vs "base". callgrind enforces the hard limit and
#    exits non-zero if any instrument breaches it; that exit code is the gate's verdict.
git checkout HEAD -- "$SRC"
run_bench --baseline=base --save-summary=json --callgrind-limits="ir=${FAIL_PCT}%"
gate_rc=$?

# Best-effort per-instrument table from the saved summaries. diff_pct is serialized as a
# string (callgrind event "Ir" = instructions). The jq walk is position-independent so it
# survives schema nesting changes; if it finds nothing the callgrind exit code still rules.
note "| Instrument | Ir Δ% | Status |"
note "|---|---:|:---:|"
table_fail=0
parsed=0
while IFS= read -r f; do
  id=$(jq -r '.id // .function_name // "?"' "$f" 2>/dev/null)
  pct=$(jq -r 'first(.. | objects | select(has("Ir")) | .Ir | objects | select(has("diffs")) | .diffs | objects | .diff_pct) // empty' "$f" 2>/dev/null)
  [ -z "$pct" ] && continue
  parsed=1
  status=$(awk -v p="$pct" -v f="$FAIL_PCT" -v w="$WARN_PCT" 'BEGIN{
    if (p+0 >= f) print "FAIL"; else if (p+0 >= w) print "WARN"; else print "ok"}')
  case "$status" in
    FAIL) icon="❌"; table_fail=1; printf '::error title=Perf regression::%s render_block Ir +%s%% (>%s%%)\n' "$id" "$pct" "$FAIL_PCT" ;;
    WARN) icon="⚠️"; printf '::warning title=Perf creep::%s render_block Ir +%s%% (>%s%%)\n' "$id" "$pct" "$WARN_PCT" ;;
    *)    icon="✅" ;;
  esac
  note "| ${id} | ${pct} | ${icon} |"
done < <(find target/iai -name summary.json 2>/dev/null)

if [ "$parsed" -eq 0 ]; then
  note "| _(summary parse unavailable — verdict from callgrind exit code)_ |  |  |"
fi
note ""

# Verdict: callgrind's own limit breach OR our table classifying a >FAIL_PCT regression.
if [ "$gate_rc" -ne 0 ] || [ "$table_fail" -ne 0 ]; then
  note "**Result: ❌ regression over ${FAIL_PCT}%.**"
  exit 1
fi
note "**Result: ✅ within ${FAIL_PCT}%.**"
exit 0
