# Contributing to reuben

## One-time setup

After cloning, point git at the shared hooks:

```sh
git config core.hooksPath .githooks
```

That's the only manual step. It activates:

- **pre-commit** — `cargo fmt --all --check` (fast; skips docs-only commits). Blocks
  commits that CI's format gate would reject.
- **pre-push** — `cargo clippy --workspace --all-targets -- -D warnings`. Runs at the
  push boundary (not every commit) so the compile cost is paid once; skips pushes that
  touch no Rust.

Both hooks mirror CI exactly and are bypassable with `--no-verify` for deliberate
exceptions. They are a local pre-flight — **CI is the real gate**; skipping setup just
means you find out at CI instead of at commit.

## Toolchain

The Rust version is pinned in [`rust-toolchain.toml`](./rust-toolchain.toml). rustup
auto-installs and uses it the first time you run any `cargo` command in the repo — you
don't pick a toolchain. Because local and CI run the *same* version, the hooks' fmt and
clippy verdicts match CI's exactly.

### Bumping the Rust version

The pinned version and the MSRV are kept **in lockstep** (see
[ADR-0023](./docs/adr/0023-toolchain-pin-and-git-hooks.md)). To move to a new Rust:

1. `channel` in `rust-toolchain.toml`
2. `rust-version` in `Cargo.toml` `[workspace.package]` — set to the **same** version

Two spots, one conceptual change. The `lockstep` CI job fails the build if they don't match,
so a forgotten second edit is caught immediately. CI then verifies the new floor for free (it builds on
the pinned toolchain, which equals the MSRV).

## Branching & release flow

The repo runs a two-branch model (see [ADR-0055](./docs/adr/0055-dev-staging-branch-strategy.md)):

- **`dev`** is the default, long-lived integration branch. **Open every PR against `dev`.**
- Pushing to `dev` runs the full CI suite and deploys the **staging** app at
  <https://dev.reuben-web-player.pages.dev>. Per-PR previews are unchanged — each PR still gets its
  own ephemeral preview URL.
- **`main` is production and ships by promotion, never by a direct merge.** Run the manual
  **[Promote dev to main](./.github/workflows/promote.yml)** workflow (Actions → *Promote dev to
  main* → Run workflow). It fast-forwards `main` to `dev` and the resulting push deploys production.

**Never commit or push directly to `main`.** A commit on `main` that isn't from `dev` diverges the
two branches and breaks the fast-forward promotion until `main` is merged back into `dev`. If a
hotfix ever *must* land on `main` directly, immediately reconcile with `git checkout dev && git merge
main` so `dev → main` stays fast-forwardable.

After the default branch switched to `dev`, run this once locally so `origin/HEAD` — and
`scripts/clean-merged-branches.sh`, which auto-targets the default branch — follow it:

```sh
git remote set-head origin -a
```
