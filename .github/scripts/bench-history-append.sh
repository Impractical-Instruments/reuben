#!/usr/bin/env bash
# Append per-commit benchmark instruction counts to the `bench-history` branch (ADR-0019, layer 1).
#
# The perf gate compares each commit to its parent and then discards the numbers — they live only in
# the job's step summary, which ages out. This script persists HEAD's absolute Ir per benched case,
# so a cross-commit trend is one command away:
#
#     git show bench-history:bench-history.jsonl
#
# It runs ONLY on direct pushes to main, in a dedicated job whose token is the single `contents:
# write` grant in CI; the gate job itself stays `contents: read` (ADR-0019). The history lives on an
# orphan branch, not main, so main's tree never churns and recording never re-triggers CI.
#
# Input: the JSONL record perf-gate.sh harvested (one {sha,commit_sha,date,run_id,layer,case,ir}
# object per case). Empty or absent input is a deliberate no-op — e.g. a commit whose bench harness
# did not compile against its baseline, so no comparison ran. That is an honest gap in the series,
# not a fabricated point.
#
# Arg 1: path to the record JSONL.
# Env:   GITHUB_TOKEN (contents:write), GITHUB_REPOSITORY (owner/repo), GITHUB_SHA.
set -euo pipefail

REC="${1:?usage: bench-history-append.sh <record.jsonl>}"
BRANCH="bench-history"
FILE="bench-history.jsonl"

if [ ! -s "$REC" ]; then
  echo "No benchmark records to append (empty or missing: $REC) — nothing to persist."
  exit 0
fi
abs_rec="$(cd "$(dirname "$REC")" && pwd)/$(basename "$REC")"
count="$(grep -c . "$abs_rec")"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cd "$work"
git init -q
git config user.name "github-actions[bot]"
git config user.email "41898282+github-actions[bot]@users.noreply.github.com"
git remote add origin "https://x-access-token:${GITHUB_TOKEN}@github.com/${GITHUB_REPOSITORY}.git"

# Append-and-push with retry. A concurrent main push can land its own history commit first and reject
# ours as a non-fast-forward; re-fetch the branch tip and re-apply our (fixed) records on top. Each
# attempt rebuilds from the latest remote state, so retries never duplicate or drop lines.
for attempt in 1 2 3 4 5; do
  rm -f "$FILE"
  if git fetch -q --depth=1 origin "$BRANCH" 2>/dev/null; then
    git checkout -q -B "$BRANCH" FETCH_HEAD
  else
    # First ever run: start the branch with no history and an empty tree.
    git checkout -q --orphan "$BRANCH"
    git rm -rqf . >/dev/null 2>&1 || true
  fi
  cat "$abs_rec" >>"$FILE"
  git add "$FILE"
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
