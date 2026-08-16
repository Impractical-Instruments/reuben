#!/bin/sh
# Point git at this repo's hook set. Run once per clone; safe to re-run, and safe to run from any
# directory inside the working tree.
#
# There is exactly one command and exactly one target, which is the point: `core.hooksPath` holds a
# single value, so a second documented way to install hooks is a way to uninstall the first.
#
# The target is `scripts/hooks/`, which is the directory `brain`'s `bootstrap.sh` configures —
# so a bootstrapped machine and a machine that ran this script end up at the same value instead of
# overwriting each other's. Bootstrap's own test is an executable regular file named after a git hook
# event directly inside that directory, which `pre-commit` and `pre-push` are; `dispatch` is not, and
# on its own would leave bootstrap reporting `inert`. Renaming either stub therefore silently
# uninstalls the set on every bootstrapped clone.
#
# This script stays the fallback for anyone who does not bootstrap, and it is what a fresh clone's
# README and CONTRIBUTING point at.
set -eu

root=$(git rev-parse --show-toplevel)
cd "$root"

# Check the target BEFORE writing the config. Writing it first and failing here would leave
# `core.hooksPath` naming a directory that does not exist — at which point git runs no hooks at all,
# in silence, which is the failure this whole arrangement exists to prevent.
if [ ! -f scripts/hooks/dispatch ]; then
    echo "install-hooks: scripts/hooks/dispatch is missing — refusing to point core.hooksPath at it" >&2
    exit 1
fi

# Git carries the executable bit, so this is a repair for a tree that lost it (an unzipped archive,
# a copy across a filesystem that does not keep the mode), not the normal path. `dispatch` refuses
# to run when a registry entry is not executable, and this is what clears that.
#
# Named precisely rather than `find scripts/hooks -type f`: a blanket chmod would mark a future README,
# a sourced helper, or a fixture executable, dirtying the tree on a script whose header promises it
# is safe to re-run. Symlinks are skipped rather than followed for the same reason, and it is the
# sharper version of it: `chmod` through a link changes the mode of the TARGET, so an entry pointing
# at a tracked file would leave that file executable, the tree dirty, and the file itself queued to
# be run as a check.
chmod +x scripts/hooks/dispatch
for hook in scripts/hooks/*.d; do
    [ -d "$hook" ] || continue
    stub=${hook%.d}
    # git looks a hook up by its exact filename and nothing else, so a registry with no stub beside
    # it is a set of checks git will never call. Refuse, rather than reporting them as installed in
    # the roster below — a roster that lists a check nothing can run is the defect this hook set
    # exists to prevent, printed in the one place a contributor looks to confirm the install worked.
    if [ ! -f "$stub" ]; then
        echo "install-hooks: $hook has no hook stub at $stub — git would never run its checks" >&2
        exit 1
    fi
    [ -L "$stub" ] || chmod +x "$stub"
    for check in "$hook"/*; do
        case "$check" in *~) continue ;; esac
        if [ -f "$check" ] && [ ! -L "$check" ]; then
            chmod +x "$check"
        fi
    done
done

previous=$(git config --get core.hooksPath || true)
if [ -n "$previous" ] && [ "$previous" != "scripts/hooks" ]; then
    echo "replacing core.hooksPath: $previous -> scripts/hooks"
fi

git config core.hooksPath scripts/hooks

echo "hooks installed: core.hooksPath -> scripts/hooks"
for hook in scripts/hooks/*.d; do
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
