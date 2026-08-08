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

# Check the target BEFORE writing the config. Writing it first and failing here would leave
# `core.hooksPath` naming a directory that does not exist — at which point git runs no hooks at all,
# in silence, which is the failure this whole arrangement exists to prevent.
if [ ! -f .githooks/dispatch ]; then
    echo "install-hooks: .githooks/dispatch is missing — refusing to point core.hooksPath at it" >&2
    exit 1
fi

# Git carries the executable bit, so this is a repair for a tree that lost it (an unzipped archive,
# a copy across a filesystem that does not keep the mode), not the normal path. `dispatch` refuses
# to run when a registry entry is not executable, and this is what clears that.
#
# Named precisely rather than `find .githooks -type f`: a blanket chmod would mark a future README,
# a sourced helper, or a fixture executable, dirtying the tree on a script whose header promises it
# is safe to re-run.
chmod +x .githooks/dispatch
for hook in .githooks/*.d; do
    [ -d "$hook" ] || continue
    stub=${hook%.d}
    [ -f "$stub" ] && chmod +x "$stub"
    for check in "$hook"/*; do
        case "$check" in *~) continue ;; esac
        [ -f "$check" ] && chmod +x "$check"
    done
done

previous=$(git config --get core.hooksPath || true)
if [ -n "$previous" ] && [ "$previous" != ".githooks" ]; then
    echo "replacing core.hooksPath: $previous -> .githooks"
fi

git config core.hooksPath .githooks

echo "hooks installed: core.hooksPath -> .githooks"
for hook in .githooks/*.d; do
    [ -d "$hook" ] || continue
    name=${hook%.d}
    printf '  %s:' "${name##*/}"
    for check in "$hook"/*; do
        case "$check" in *~) continue ;; esac
        [ -f "$check" ] || continue
        printf ' %s' "${check##*/}"
    done
    printf '\n'
done
