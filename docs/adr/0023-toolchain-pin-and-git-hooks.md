# Pinned toolchain, lockstep MSRV, and shared git hooks

## Context

CI ([.github/workflows/ci.yml](../../.github/workflows/ci.yml)) gates every PR on
`cargo fmt --all --check`, `cargo clippy ... -D warnings`, and tests. A prior local-only
`.git/hooks/pre-commit` ran the fmt check to catch failures before pushing — but it lived
in `.git/hooks/`, so it was unversioned, single-machine, and invisible to anyone cloning
the repo. reuben is open source; a newcomer's first contribution shouldn't trip a CI fmt
gate that a one-line setup could have caught.

Sharing the hook surfaced a deeper problem: a hook runs the *local* rustfmt/clippy. CI ran
`dtolnay/rust-toolchain@stable` — a *floating* version. Local tool ≠ CI tool means the
shared hook could pass locally and still fail CI (rustfmt), and — more sharply — a new
stable Rust release could break CI with **zero code change** when a freshly-stabilized
clippy lint fires under `-D warnings`. The hook is only trustworthy if local and CI run the
*same* toolchain.

## Decision

### Pin the toolchain; make it the single source of truth

`rust-toolchain.toml` pins the exact version (`channel = "1.96.0"`, today's stable — a
zero-behavior-change snapshot) plus `components = ["rustfmt", "clippy"]`. rustup auto-honors
this file: any `cargo` call in the repo uses it and auto-installs it if missing. Local dev,
every machine, newcomers, and CI now run one identical toolchain, so a hook's verdict equals
CI's. The prize is less about fmt (rarely changes across stable) than about **reproducible
clippy** — no more "new Rust silently breaks CI."

CI reads the same file rather than declaring a version: the `dtolnay/rust-toolchain` action
was dropped in favor of `rustup toolchain install` (no arg → installs the toml's channel +
components). With the toml present the action earned nothing it provides — rustup already
auto-provisions from the file. This keeps the version in **one** place.

### MSRV in lockstep with the pin

`Cargo.toml` `[workspace.package]` declares `rust-version = "1.96.0"`, equal to the pin. The
rule: **MSRV always equals the pinned channel; they are bumped together.** This keeps the
MSRV self-verifying for free — CI builds on the pinned toolchain, which *is* the MSRV, so no
separate MSRV job is needed.

The trade is explicit: an MSRV that tracks the pin is a *floor that declares "you need the
version I develop on,"* not a promise of support across a range of older Rust. The day reuben
wants to support older toolchains, the lockstep rule is lifted and a dedicated CI job pinned
to the (now lower) MSRV is added — without that job, an MSRV below the pin is an unverified
claim that rots the moment a newer-Rust-only feature is used.

### Shared hooks via committed `.githooks/` + `core.hooksPath`

Hooks live in version-controlled [`.githooks/`](../../.githooks); contributors run
`git config core.hooksPath .githooks` once ([CONTRIBUTING.md](../../CONTRIBUTING.md)).

- **pre-commit** — `cargo fmt --all --check`, skipped on docs-only commits. Fast, so commits
  stay cheap.
- **pre-push** — `cargo clippy --workspace --all-targets -- -D warnings`, at the push
  boundary (not per-commit), skipped when the pushed commits touch no Rust. clippy compiles,
  so paying that cost once per push — where CI would catch it anyway — beats taxing every
  commit and provoking habitual `--no-verify`.

## Alternatives considered

- **cargo-husky (auto-install hooks via a build script).** The one feature it adds is dodging
  the single `core.hooksPath` setup line, at the cost of a dev-dependency whose `build.rs`
  mutates `.git/hooks` on every build — a side effect outside `OUT_DIR` that some Rust devs
  distrust. Rejected: the hooks are convenience over CI (which is the real gate), so a missed
  setup costs "find out at CI, not at commit" — not worth a build-time side-effect dependency.
- **Keeping `dtolnay/rust-toolchain`, pinned to the version.** Works, but duplicates the
  version into CI (×2 jobs) on top of the toml — three bump-spots. With the toml driving
  rustup, the action is redundant. Rejected for single-source.
- **Decoupled MSRV (lower than pin) now.** The honest wide-support path, but it requires a
  dedicated MSRV CI job to avoid rot, for a project with no current older-Rust users.
  Deferred until a real need appears.

## Consequences

- Bumping Rust = edit two spots, kept equal: `channel` in `rust-toolchain.toml` and
  `rust-version` in `Cargo.toml`. A grep catches both; CI re-verifies the floor automatically.
- Newcomers get the exact toolchain with no choice, and the fmt/clippy gates locally before
  CI — after one setup line.
- An older-Rust support story is explicitly out of scope until the lockstep rule is lifted.
