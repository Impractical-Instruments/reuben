#!/usr/bin/env bash
# Agent-surface eval gate — deterministic tier. see rules: agent-mcp
#
# Sibling of perf-gate.sh, same idiom, different axis: that one watches what the ENGINE costs the
# CPU, this one watches what the AGENT SURFACE costs a model. Grounding-token bloat is a real and
# currently invisible regression — anyone adding a paragraph to a tool description or the authoring
# guide makes every turn of every task more expensive, and no existing check sees it.
#
# What it measures (#592's yardstick): (a) grounding tokens the sidecar hands back, (b) failed
# `validate` rounds, (c) freehand-JSON characters the model must emit. Each task carries a
# hand-written REFERENCE SOLUTION — the ideal call sequence a perfect model would make — replayed
# against the real sidecar with NO inference. So this is the surface's COST FLOOR, and a prototype's
# claim on the #574 map is checkable before a single token is bought.
#
#   FAIL (exit 1): any metric regresses >= 10% vs the baseline, or a reference solution stops passing.
#   WARN:          3%..10% — job summary + GH annotation, non-blocking.
#
# HOW THE BASELINE IS BUILT — the mirror image of perf-gate.sh's swap. There, the HARNESS is held
# fixed and the ENGINE SOURCE moves to the baseline ref. Here the same rule applies, and the split is
# what makes the comparison mean anything:
#
#   - `eval/` (the harness: tasks, reference solutions, tokenizer) is NEVER swapped. Both sides are
#     measured by the PR's own harness, so a task added in this PR still measures, and a scoring
#     change applies symmetrically instead of manufacturing a Δ out of its own diff.
#   - The MEASURED SURFACE is swapped: the sidecar and everything whose text reaches a model —
#     crates/ (tool descriptions, `instructions`, report shapes), docs/agents/ (the authoring guide
#     and vocabulary served as resources), and instruments/ (the fixtures the tasks read). Those move
#     together for the same reason perf-gate moves src+fixtures together: the guide is served through
#     the engine, so a half-swapped tree measures a chimera.
#
# A baseline whose sidecar does not BUILD is a skip, not a pass-by-default — the same
# compile-vs-runtime split perf-gate.sh draws, for the same reason: silently skipping is how a broken
# gate slips through green.
#
# Arg 1: baseline commit SHA (empty => no comparison, absolute numbers only).
set -uo pipefail

BASE_SHA="${1:-}"
SUMMARY="${GITHUB_STEP_SUMMARY:-/dev/stdout}"
WORK="${GITHUB_WORKSPACE:-$PWD}"
RECORD="$WORK/eval-history-record.jsonl"
HEAD_JSON="$WORK/eval-head.json"
BASE_JSON="$WORK/eval-base.json"

# The measured surface: everything whose bytes can reach a model, swapped together to the baseline.
# `eval/` is deliberately absent — see the header.
SURFACE=(crates docs/agents instruments)

note() { printf '%s\n' "$*" >>"$SUMMARY"; }

# Identity for the trend record. Resolved once: HEAD never moves, only the worktree does.
export EVAL_SHA="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
export EVAL_COMMIT_SHA="$(git rev-parse HEAD 2>/dev/null || echo unknown)"
export EVAL_DATE="$(git show -s --format=%cI HEAD 2>/dev/null || echo unknown)"

build_sidecar() { cargo build -p reuben-mcp --quiet; }
run_eval() { python3 -m reuben_eval.gate "$@"; }

# Make the worktree under each surface path match `$ref` EXACTLY, deletions included.
#
# A plain `git checkout $ref -- $path` only copies entries that exist in $ref; it never removes a
# worktree file that $ref lacks. That silently breaks this gate on a PR that DELETES a surface file:
# swapping to the baseline resurrects the file, and swapping back to HEAD leaves the baseline copy on
# disk — so the HEAD sidecar would serve a deleted `docs/agents/**` resource or load a deleted
# `instruments/**` fixture, corrupting the HEAD measurement and the next baseline. (perf-gate.sh's
# swap has the same blind spot, but its swapped tree is source that goes dead, not live inputs read
# back at measure time.) So: unstage the path, restore $ref's tree, then `git clean` the leftovers
# $ref doesn't have. For a no-op PR (bytes identical across refs) this is a wash — nothing to clean,
# nothing rebuilds — preserving the cheap common case.
#
# Per-path, guarded on existence in $ref, so a baseline predating one of these directories restores
# the rest instead of aborting the whole swap.
swap_surface() {
  local ref="$1" path
  for path in "${SURFACE[@]}"; do
    if git cat-file -e "${ref}:${path}" 2>/dev/null; then
      git rm -rq --cached --ignore-unmatch -- "$path" >/dev/null 2>&1 || true
      git checkout "$ref" -- "$path"
      # -fd removes untracked leftovers; no -x, so .gitignore (target/, etc.) is untouched.
      git clean -fdq -- "$path"
    fi
  done
}

if [ ! -d "$WORK/eval" ]; then
  printf '::error title=eval harness missing::%s/eval does not exist\n' "$WORK"
  exit 1
fi

# 1) Baseline side. Swap the surface, rebuild the sidecar against it, measure with THIS PR's harness.
#    `--no-gate` because the baseline is compared against nothing; we only want its numbers.
have_baseline=0
if [ -n "$BASE_SHA" ] && git cat-file -e "${BASE_SHA}^{commit}" 2>/dev/null; then
  swap_surface "$BASE_SHA"
  if build_sidecar; then
    if (cd eval && run_eval --no-gate --json "$BASE_JSON" --summary /dev/null) >/dev/null 2>&1; then
      have_baseline=1
    else
      # Built but wouldn't measure: the PR's harness needs a surface the baseline doesn't have (a
      # task bound to a tool that didn't exist yet). No apples-to-apples baseline; report absolutes.
      printf '::warning title=Eval baseline unmeasurable::the PR harness could not measure the baseline surface — absolute numbers only\n'
      note "⚠️ The PR's harness could not measure the baseline surface (a task likely depends on something this PR adds). Absolute numbers only, no comparison."
      note ""
    fi
  else
    printf '::warning title=Eval baseline build failed::baseline reuben-mcp did not build — no comparison for this run\n'
    note "⚠️ The baseline \`reuben-mcp\` does not build — no comparison this run."
    note ""
  fi
  swap_surface HEAD
fi

# 2) PR side. Rebuild against HEAD's surface and gate. A HEAD sidecar that won't build is a hard
#    failure: `check` would red anyway, but this gate must not report green off a stale binary.
if ! build_sidecar; then
  printf '::error title=Sidecar build failed::reuben-mcp did not build at HEAD — the eval gate cannot certify anything\n'
  note "❌ \`reuben-mcp\` does not build at HEAD — the eval gate could not run. Not a pass."
  exit 1
fi

compare_args=()
[ "$have_baseline" -eq 1 ] && compare_args=(--compare "$BASE_JSON")

cd eval
run_eval "${compare_args[@]}" --json "$HEAD_JSON" --history "$RECORD"
rc=$?
cd "$WORK"

if [ "$rc" -ne 0 ]; then
  note "**Result: ❌ the agent surface got more expensive, or a reference solution stopped passing.**"
fi
exit "$rc"
