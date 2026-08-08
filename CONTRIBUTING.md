# Contributing to reuben

## One-time setup

After cloning, install the hook set:

```sh
./scripts/install-hooks.sh
```

That's the only manual step, and it is the only one there will ever be: the script points
`core.hooksPath` at [`.githooks/`](./.githooks), and everything else registers itself there.

### The hook set

`core.hooksPath` names one directory and git looks a hook up by its exact filename, so a hook
type is one file. [`.githooks/pre-commit`](./.githooks/pre-commit) and
[`.githooks/pre-push`](./.githooks/pre-push) are therefore two-line stubs that hand off to
[`.githooks/dispatch`](./.githooks/dispatch), which runs every executable under
`.githooks/<hook>.d/` in filename order and stops at the first failure. What is installed today:

- **pre-commit**
  - `.githooks/pre-commit.d/10-rules-refs` — `check_rules_refs.py` over the working tree: no ADR
    number in code, no comment citing an issue or a rule anchor, every `see rules:` pointer
    resolving.
  - `.githooks/pre-commit.d/20-rust-fmt` — `cargo fmt --all --check` (fast; skips docs-only
    commits). Blocks commits that CI's format gate would reject.
  - `.githooks/pre-commit.d/30-rules-index` — `check_rules_derive.py --write` when the commit
    touches `docs/rules/`, re-staging the regenerated index so the fix lands in the same commit.
- **pre-push**
  - `.githooks/pre-push.d/10-rust-clippy` — `cargo clippy --workspace --all-targets -- -D
    warnings`. Runs at the push boundary (not every commit) so the compile cost is paid once;
    skips pushes that touch no Rust.

Every check mirrors CI exactly, and `--no-verify` bypasses the whole set for deliberate
exceptions. They are a local pre-flight — **CI is the real gate**; skipping setup just means you
find out at CI instead of at commit.

### Adding a check

Drop an executable file in `.githooks/pre-commit.d/` or `.githooks/pre-push.d/` and re-run
`./scripts/install-hooks.sh` if the tree lost the executable bit. There is nothing to register and
nothing to displace — which is the reason for the indirection, because the alternative is a second
hooks directory that silently disables the first.

Two conventions the numeric prefix carries:

- **Read-only checks take the low numbers; a check that writes takes a high one.** `30-rules-index`
  regenerates and re-stages a file, and a read-only check failing after it would leave you a
  modified, staged file you never touched and were never told about.
- **A check is handed the hook's own arguments and a verbatim replay of the hook's stdin**, so
  every pre-push check sees the same pushed refs. It runs from the working-tree root and may stage
  files.

## Toolchain

The Rust version is pinned in [`rust-toolchain.toml`](./rust-toolchain.toml). rustup
auto-installs and uses it the first time you run any `cargo` command in the repo — you
don't pick a toolchain. Because local and CI run the *same* version, the hooks' fmt and
clippy verdicts match CI's exactly.

### Bumping the Rust version

The pinned version and the MSRV are kept **in lockstep** (see
[web-product-process](./docs/rules/web-product-process.md)). To move to a new Rust:

1. `channel` in `rust-toolchain.toml`
2. `rust-version` in `Cargo.toml` `[workspace.package]` — set to the **same** version

Two spots, one conceptual change. The `lockstep` CI job fails the build if they don't match,
so a forgotten second edit is caught immediately. CI then verifies the new floor for free (it builds on
the pinned toolchain, which equals the MSRV).

## Branching & release flow

The repo runs a two-branch model (see [web-product-process](./docs/rules/web-product-process.md)):

- **`dev`** is the default, long-lived integration branch. **Open every PR against `dev`.**
- Pushing to `dev` runs the full CI suite. (The staging/preview *deploys* that once lived here
  moved out with the web player — they now run in the private `reuben-web` repo, which pins this
  one as a submodule. The promotion model below is unchanged.)
- **`main` is production and ships by promotion, never by a direct merge.** Run the manual
  **[Promote dev to main](./.github/workflows/promote.yml)** workflow (Actions → *Promote dev to
  main* → Run workflow). It fast-forwards `main` to `dev` and the resulting push deploys production.

**Never commit or push directly to `main`.** A commit on `main` that isn't from `dev` diverges the
two branches and breaks the fast-forward promotion until `main` is merged back into `dev`. If a
hotfix ever *must* land on `main` directly, immediately reconcile with `git checkout dev && git merge
main` so `dev → main` stays fast-forwardable.

After the default branch switched to `dev`, run this once locally so `origin/HEAD` follows it.
A clone that still points `origin/HEAD` at `main` will resolve `origin/HEAD` and anything built
on it against production rather than the integration branch:

```sh
git remote set-head origin -a
```
