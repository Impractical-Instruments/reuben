# Why: A single `rust-toolchain.toml` pins the exact toolchain as the one source of truth, with the workspace MSRV held in lockstep with the pinned channel and enforced in CI.

[Rule](../../web-product-process.md#toolchain-pin)

CI gates every PR on `cargo fmt --check`, `cargo clippy -D warnings`, and tests. If CI floats a
*stable* toolchain (`@stable`) while contributors run whatever they have locally, two things break:
a [shared hook](shared-git-hooks.md) runs the *local* rustfmt/clippy and can pass locally yet fail
CI, and — more sharply — a new stable Rust release can break CI with **zero code change** the moment
a freshly-stabilized clippy lint fires under `-D warnings`. The hook is only trustworthy if local and
CI run the *same* toolchain. So `rust-toolchain.toml` pins the exact version plus `rustfmt`/`clippy`;
rustup auto-honors it, so every `cargo` call in the repo — local, newcomer, CI — uses one identical
toolchain and a hook's verdict equals CI's. The prize is less fmt (rarely changes) than
**reproducible clippy**: no more "new Rust silently breaks CI." CI reads the same file rather than
declaring a version, keeping it in one place.

MSRV is held in **lockstep**: `rust-version` equals the pinned channel, bumped together. This makes
the MSRV self-verifying for free — CI builds on the pinned toolchain, which *is* the MSRV, so no
separate build-against-MSRV job is needed. The one thing a human can forget on a bump is the equality
itself, so it is enforced, not trusted: a fast, toolchain-free CI job asserts `channel` ==
`rust-version` and fails on drift. The trade is explicit — a lockstep MSRV is a *floor* ("you need
the version I develop on"), not a promise of support across older Rust; the day older toolchains
matter, the lockstep rule is lifted and a dedicated lower-MSRV CI job is added, without which an MSRV
below the pin is an unverified claim that rots the first time a newer-Rust-only feature is used.

Distilled from: ADR-0023
