#!/bin/bash
# PostToolUse hook: format Rust files the agent just edited.
#
# The repo gates on `cargo fmt --all --check` in CI and in the pre-commit hook
# (see CONTRIBUTING.md). Those catch unformatted code at the commit/CI boundary;
# this hook closes the loop one step earlier by running rustfmt on each .rs file
# as it is written, so an agent never leaves unformatted code in the tree between
# an edit and a commit. It mirrors CI's verdict because it uses the pinned
# toolchain's rustfmt (rust-toolchain.toml) at the workspace edition.
set -euo pipefail

# The hook payload arrives as JSON on stdin; the edited path is tool_input.file_path.
# (Edit/Write/MultiEdit all carry it.) No jq match -> nothing to do.
file="$(jq -r '.tool_input.file_path // empty' 2>/dev/null)" || exit 0
[ -n "$file" ] || exit 0

# Only Rust source, and only if it still exists (an edit could have moved it).
case "$file" in
  *.rs) ;;
  *) exit 0 ;;
esac
[ -f "$file" ] || exit 0

# rustfmt ships with the pinned toolchain; if it is somehow absent, stay silent
# rather than failing the tool call — CI is still the real gate.
command -v rustfmt >/dev/null 2>&1 || exit 0

# Edition must match the workspace ([workspace.package] edition in Cargo.toml);
# rustfmt on a bare file has no Cargo context to infer it from.
rustfmt --edition 2021 "$file" >/dev/null 2>&1 || exit 0
