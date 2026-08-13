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

The floor acquired a second job when operator self-registration moved to a link-time table
([operator self-registration](../composition-operators/operator-self-registration.md)). That table
lives in its own `link_section` and nothing calls into it, so what keeps it is the linker's
business: GNU ld (bfd) retains an input section that a `__start_`/`__stop_` symbol references, but
lld has garbage-collected exactly that case since LLVM 13 (`-z start-stop-gc`), so under lld the
section survives only when flagged `SHF_GNU_RETAIN`. rustc emits that flag from **1.89**, where
plain `#[used]` became `#[used(linker)]` on ELF (rust-lang/rust#140872 — carried in no release
note, so the mechanism is the citation rather than a changelog). **1.90** is where it starts to
bite: that release made rust-lld the default linker on `x86_64-unknown-linux-gnu` and, in the same
release, dropped the `-znostart-stop-gc` workaround (rust-lang/rust#140525) added a few versions
earlier naming `linkme` as its reason (rust-lang/rust#137685).

Upstream sequenced those two so that no released stable leaves a gap on that target, which is why
this is a thing to **check on a rollback, not a number to fear**: what has to keep lining up is the
pair — which linker runs, and whether `#[used]` still means `used(linker)` there. It does not line
up uniformly across ELF: 1.92 narrows `#[used]` back to `#[used(compiler)]` on illumos
(rust-lang/rust#147117). What this does *not* risk is worth saying, because the wrong model is the
expensive one: losing the elements loses the whole section, which un-defines the boundary symbols
the declaring crate reads, so the build stops at `undefined symbol: __start_linkme_…`
(dtolnay/linkme#63). It does not link clean with an empty registry — that silent mode belonged to
the constructor-based mechanism this replaced, and is the reason it was replaced.

Distilled from: ADR-0023
