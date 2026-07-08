#!/usr/bin/env bash
#
# Delete local branches that have already been merged into the default branch,
# including branches that were *squash* merged (which `git branch --merged`
# cannot detect, because the squashed commit has a different SHA and tree
# history than the original branch).
#
# Detection:
#   - Normal / fast-forward / rebase merges: `git branch --merged`.
#   - Squash merges: replay the branch as a single commit on top of the
#     merge-base and ask `git cherry` whether that patch is already present
#     in the target. If the diff is empty ("-"), the branch is fully merged.
#
# Usage:
#   scripts/clean-merged-branches.sh            # dry run: show what would go
#   scripts/clean-merged-branches.sh --delete   # actually delete
#   scripts/clean-merged-branches.sh --delete --target develop
#
set -euo pipefail

TARGET=""
DO_DELETE=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --delete|-d) DO_DELETE=1; shift ;;
    --target|-t) TARGET="${2:-}"; shift 2 ;;
    -h|--help)
      grep '^#' "$0" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Protected branches that are never deleted.
PROTECTED=(main master develop dev release)

# Resolve the default/target branch.
if [[ -z "$TARGET" ]]; then
  if head=$(git symbolic-ref --quiet refs/remotes/origin/HEAD 2>/dev/null); then
    TARGET="${head##*/}"
  fi
  for candidate in main master develop; do
    [[ -n "${TARGET:-}" ]] && break
    git show-ref --verify --quiet "refs/heads/$candidate" && TARGET="$candidate"
  done
fi

if [[ -z "${TARGET:-}" ]] || ! git show-ref --verify --quiet "refs/heads/$TARGET"; then
  echo "Could not determine a valid target branch (got: '${TARGET:-}')." >&2
  echo "Pass one explicitly with --target <branch>." >&2
  exit 1
fi

current=$(git rev-parse --abbrev-ref HEAD)

echo "Target branch: $TARGET"
[[ "$DO_DELETE" -eq 1 ]] && echo "Mode: DELETE" || echo "Mode: dry run (pass --delete to remove)"
echo

is_protected() {
  local b="$1"
  [[ "$b" == "$TARGET" || "$b" == "$current" ]] && return 0
  for p in "${PROTECTED[@]}"; do
    [[ "$b" == "$p" ]] && return 0
  done
  return 1
}

# Branch is squash-merged if replaying it as one commit on top of the
# merge-base produces a patch already contained in TARGET.
is_squash_merged() {
  local branch="$1"
  local base tree
  base=$(git merge-base "$TARGET" "$branch") || return 1
  tree=$(git rev-parse "$branch^{tree}")
  # commit-tree makes a throwaway commit; git cherry prints "-" when the
  # equivalent change already exists downstream.
  local synthetic
  synthetic=$(git commit-tree "$tree" -p "$base" -m _)
  [[ "$(git cherry "$TARGET" "$synthetic")" == "-"* ]]
}

to_delete=()
while IFS= read -r branch; do
  is_protected "$branch" && continue

  if git branch --merged "$TARGET" --format='%(refname:short)' | grep -qx "$branch"; then
    to_delete+=("$branch|merged")
  elif is_squash_merged "$branch"; then
    to_delete+=("$branch|squash-merged")
  fi
done < <(git for-each-ref --format='%(refname:short)' refs/heads/)

if [[ ${#to_delete[@]} -eq 0 ]]; then
  echo "No merged branches to clean up."
  exit 0
fi

for entry in "${to_delete[@]}"; do
  branch="${entry%%|*}"
  reason="${entry##*|}"
  if [[ "$DO_DELETE" -eq 1 ]]; then
    # -D because squash-merged branches are not recognized by git's own
    # merge check and -d would refuse them.
    git branch -D "$branch" && echo "deleted  $branch ($reason)"
  else
    printf 'would delete  %-45s (%s)\n' "$branch" "$reason"
  fi
done
