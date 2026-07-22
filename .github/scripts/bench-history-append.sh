#!/usr/bin/env bash
# Append per-commit benchmark instruction counts to a per-branch orphan trend (layer 1).
# see rules: web-product-process
#
# The perf gate compares each commit to its parent and then discards the numbers — they live only in
# the job's step summary, which ages out. This script persists HEAD's absolute Ir per benched case,
# so a cross-commit trend is one command away:
#
#     git show bench-history:bench-history.jsonl        # main's trend
#     git show bench-history-dev:bench-history.jsonl    # dev's trend
#
# It also re-renders the dashboard (layer 2, bench-dashboard.py) — a README.md + SVG charts
# committed beside the JSONL, so browsing the branch on GitHub *is* the trend view.
#
# It runs ONLY on direct pushes to main and dev, in a dedicated job whose token is the single
# `contents: write` grant in CI; the gate job itself stays `contents: read`. Each source
# branch keeps its own isolated orphan trend (main -> bench-history, dev -> bench-history-dev), so
# main's and dev's series never mix, and neither the source branches' trees nor CI are re-triggered.
#
# Input: the JSONL record perf-gate.sh harvested (one {sha,commit_sha,date,run_id,layer,case,ir}
# object per case). Empty or absent input is a deliberate no-op — e.g. a commit whose bench harness
# did not compile against its baseline, so no comparison ran. That is an honest gap in the series,
# not a fabricated point.
#
# It carries TWO series on the same branch (#598): `bench-history.jsonl` (what the engine costs the
# CPU) and `eval-history.jsonl` (what the agent surface costs a model). One place to look — a PR
# trading engine instructions for grounding tokens is visible in a single view — and the record
# shapes stay distinct, so `ir` never holds a token count. Either input may be absent independently:
# the two gates are path-filtered differently and skip on different commits, so a commit that
# recorded only one series appends only that one rather than punching a hole in both.
#
# Arg 1: path to the bench record JSONL.
# Arg 2: target orphan branch (default `bench-history`).
# Arg 3: human branch label the dashboard prints in its copy (default `main`).
# Arg 4: path to the eval record JSONL (optional).
# Env:   GITHUB_TOKEN (contents:write), GITHUB_REPOSITORY (owner/repo), GITHUB_SHA.
set -euo pipefail

REC="${1:-}"
BRANCH="${2:-bench-history}"
LABEL="${3:-main}"
EVAL_REC="${4:-}"
# Same filenames on every trend branch, so `git show <branch>:bench-history.jsonl` and the
# dashboard's readers are identical regardless of which branch's series is being appended.
FILE="bench-history.jsonl"
EVAL_FILE="eval-history.jsonl"
DASHBOARD="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/bench-dashboard.py"

# Resolve an optional record to an absolute path, or to the empty string when there is nothing to
# append. Empty/absent is a deliberate no-op — an honest gap in that series, not a fabricated point.
resolve() {
  [ -n "${1:-}" ] && [ -s "$1" ] || { printf ''; return; }
  printf '%s/%s' "$(cd "$(dirname "$1")" && pwd)" "$(basename "$1")"
}

abs_rec="$(resolve "$REC")"
abs_eval="$(resolve "$EVAL_REC")"

if [ -z "$abs_rec" ] && [ -z "$abs_eval" ]; then
  echo "No records to append (bench: '${REC:-none}', eval: '${EVAL_REC:-none}') — nothing to persist."
  exit 0
fi
# Non-blank lines in an optional record. `grep -c` exits 1 on a zero count, which `set -e` would
# treat as fatal, so the count is taken defensively and defaults to 0.
lines_in() {
  local n=0
  [ -n "${1:-}" ] && n="$(grep -c . "$1" 2>/dev/null || true)"
  printf '%s' "${n:-0}"
}
count=$(( $(lines_in "$abs_rec") + $(lines_in "$abs_eval") ))

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"
git init -q
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
# HIST_REMOTE exists so this script can be exercised against a local bare repo — the append/render/
# retry path is where the two-series logic lives, and asserting it works is not the same as running
# it. CI never sets it, so the GitHub URL stays the default.
git remote add origin \
  "${HIST_REMOTE:-https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_REPOSITORY}.git}"

# Append-and-push with retry. A concurrent main push can land its own history commit first and reject
# ours as a non-fast-forward; re-fetch the branch tip and re-apply our (fixed) records on top. Each
# attempt rebuilds from the latest remote state, so retries never duplicate or drop lines.
for attempt in 1 2 3 4 5; do
  rm -f "$FILE" "$EVAL_FILE"
  if git fetch -q --depth=1 origin "$BRANCH" 2>/dev/null; then
    git checkout -q -B "$BRANCH" FETCH_HEAD
  else
    # First ever run: start the branch with no history and an empty tree.
    git checkout -q --orphan "$BRANCH"
    git rm -rqf . >/dev/null 2>&1 || true
  fi
  if [ -n "$abs_rec" ]; then cat "$abs_rec" >>"$FILE"; fi
  if [ -n "$abs_eval" ]; then cat "$abs_eval" >>"$EVAL_FILE"; fi
  # Re-render the dashboard (layer 2) over the full series so browsing the branch on
  # GitHub shows the trend. Best-effort BOTH ways: a render bug must never lose the data point,
  # and a failed render must never commit the deletion (or a half-written replacement) of the
  # previous dashboard — wipe whatever the failed run left and restore the branch-tip render,
  # path by path (a first-ever run has neither in the index; checkout of a missing path fails
  # alone without blocking the other).
  # The dashboard is driven by the bench series and picks up `eval-history.jsonl` from the same
  # directory when present. An eval-only commit on a branch with no bench series yet has nothing to
  # drive it, so the render is skipped rather than crashed.
  rm -rf README.md charts
  if [ ! -s "$FILE" ]; then
    echo "No bench series on '${BRANCH}' yet — appended eval records, skipping the dashboard render."
  elif ! python3 "$DASHBOARD" "$FILE" . "$LABEL"; then
    echo "WARNING: dashboard render failed — restoring the previous dashboard, appending data only." >&2
    rm -rf README.md charts
    for p in README.md charts; do git checkout -q -- "$p" 2>/dev/null || true; done
  fi
  git add -A
  git commit -q -m "bench: record ${count} data point(s) @ ${GITHUB_SHA:0:12}"
  if git push -q origin "$BRANCH"; then
    echo "Appended ${count} record(s) to '${BRANCH}'."
    exit 0
  fi
  echo "Push rejected (attempt ${attempt}/5) — re-fetching '${BRANCH}' and retrying."
  sleep $((attempt * 2))
done

echo "Failed to append bench history after 5 attempts." >&2
exit 1
