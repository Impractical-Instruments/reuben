# Why: Git hooks are version-controlled under `scripts/hooks/` — the one directory the company bootstrap configures — and shared via `core.hooksPath`, as a convenience ahead of the authoritative CI gate.

[Rule](../../web-product-process.md#shared-git-hooks)

reuben is open source; a newcomer's first contribution shouldn't trip a CI fmt gate
that a one-line setup could have caught. So hooks live in version-controlled `scripts/hooks/`, wired
via a single `git config core.hooksPath scripts/hooks` line. **pre-commit** runs
`cargo fmt --all --check`,
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

**`scripts/hooks/` is the directory because something outside this repo configures it.** The company
bootstrap points `core.hooksPath` at exactly that path and looks nowhere else; a repo that keeps its
hooks anywhere else reports as having none, and its checks never fire on a machine that was
bootstrapped rather than hand-installed. `core.hooksPath` holds one value, so two conventions is not a
richer choice — it is a coin toss over whose hooks run. The install script stays as the fallback for
someone who does not bootstrap, and sets the same value.

Two consequences worth carrying. The **entry points have to be named after git hook events** —
`pre-commit`, `pre-push` — and be executable, because that is bootstrap's test for a directory holding
hooks at all; the dispatcher beside them is not a hook name and would leave bootstrap configuring
nothing. And the **dispatcher-plus-fragments design is independent of the directory**: adding a check
must never mean choosing between existing ones, which is why each check is its own file under
`<hook>.d/` and the hook git looks up is a stub.

An earlier version of this rule named `.githooks/`, on agent-authored reasoning that read as settled
doctrine. Charlie overturned that on 2026-08-12: it was never his decision, and a preference nobody
stated does not become law by being written down confidently. What survived the reversal is
everything above about *why there is one installed set*; only the directory changed.

Distilled from: ADR-0023
