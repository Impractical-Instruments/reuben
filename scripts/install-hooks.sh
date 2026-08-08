#!/bin/sh
# Point git at this repo's hook set. Run once per clone; safe to re-run, and safe to run from any
# directory inside the working tree.
#
# There is exactly one command and exactly one target, which is the point: `core.hooksPath` holds a
# single value, so a second documented way to install hooks is a way to uninstall the first.
# see rules: web-product-process
set -eu

root=$(git rev-parse --show-toplevel)
cd "$root"

previous=$(git config --get core.hooksPath || true)
if [ -n "$previous" ] && [ "$previous" != ".githooks" ]; then
    echo "replacing core.hooksPath: $previous -> .githooks"
fi

git config core.hooksPath .githooks

# Git carries the executable bit, so this is a repair for a tree that lost it (an unzipped archive,
# a copy across a filesystem that does not keep the mode), not the normal path. `dispatch` skips a
# registry entry that is not executable, so a missing bit is a silently disabled check.
find .githooks -type f -exec chmod +x {} +

echo "hooks installed: core.hooksPath -> .githooks"
for hook in .githooks/*.d; do
    [ -d "$hook" ] || continue
    name=${hook%.d}
    printf '  %s:' "${name##*/}"
    for check in "$hook"/*; do
        [ -f "$check" ] && [ -x "$check" ] || continue
        printf ' %s' "${check##*/}"
    done
    printf '\n'
done
