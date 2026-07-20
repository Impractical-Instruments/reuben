# Why: Git hooks are version-controlled under `.githooks/` and shared via `core.hooksPath` — pre-commit fmt, pre-push clippy — as a convenience ahead of the authoritative CI gate.

[Rule](../../web-product-process.md#shared-git-hooks)

A prior pre-commit hook lived in `.git/hooks/` — unversioned, single-machine, invisible to anyone
cloning the repo. reuben is open source; a newcomer's first contribution shouldn't trip a CI fmt gate
that a one-line setup could have caught. So hooks live in version-controlled `.githooks/`, wired via
a single `git config core.hooksPath .githooks` line. **pre-commit** runs `cargo fmt --all --check`,
skipped on docs-only commits so commits stay cheap; **pre-push** runs
`cargo clippy --workspace --all-targets -- -D warnings` at the push boundary (not per-commit, skipped
when no Rust is pushed) — clippy compiles, so paying that cost once per push beats taxing every commit
and provoking habitual `--no-verify`.

The hooks are **convenience over the real gate**, which is always CI: a missed setup line costs only
"find out at CI, not at commit," never a broken merge. That framing settled the alternatives —
`cargo-husky` was rejected because auto-installing hooks via a `build.rs` that mutates `.git/hooks`
on every build is a side-effect outside `OUT_DIR` that some Rust devs distrust, and it buys only the
dodging of one setup line. The hooks are trustworthy precisely because the
[pinned toolchain](toolchain-pin.md) makes their local fmt/clippy verdict identical to CI's.

Distilled from: ADR-0023
