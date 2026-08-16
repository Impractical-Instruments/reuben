# Contributing to reuben

## One-time setup

After cloning, install the hook set — unless `brain`'s `bootstrap.sh` has already run on this
machine, which configures it for you:

```sh
./scripts/install-hooks.sh
```

The script points `core.hooksPath` at [`scripts/hooks/`](./scripts/hooks), which is the same
directory bootstrap configures, so the two routes agree rather than overwriting each other. Once it is
set, a new check registers itself there and needs no further setup.

**This is not the only manual step**, and an earlier version of this section said it was. A clone also
wants `python3` on `PATH` for the checks that are not `cargo` (below), and the doctrine regeneration
check additionally wants the `impractical-doctrine` plugin and a token that can reach `brain` — it
warns and steps aside without them. What the one command buys is the hook set, not a finished
environment.

### The hook set

`core.hooksPath` names one directory and git looks a hook up by its exact filename, so a hook
type is one file. [`scripts/hooks/pre-commit`](./scripts/hooks/pre-commit) and
[`scripts/hooks/pre-push`](./scripts/hooks/pre-push) are therefore two-line stubs that hand off to
[`scripts/hooks/dispatch`](./scripts/hooks/dispatch), which runs every executable under
`scripts/hooks/<hook>.d/` in filename order and stops at the first failure. What is installed today:

- **pre-commit**
  - `scripts/hooks/pre-commit.d/10-rules-refs` — `check_rules_refs.py` over the working tree: no ADR
    number in code, no comment citing an issue or a rule anchor, every `see rules:` pointer
    resolving.
  - `scripts/hooks/pre-commit.d/15-adr-numbers` — `check_adr_numbers.py` over the working tree: no
    ADR number is carried by two decisions, live or long since folded away. **Warns and steps aside
    in a shallow clone**, which cannot say which numbers were ever issued; CI's `adr-numbers` job
    checks out full-depth and is the authority.
  - `scripts/hooks/pre-commit.d/20-rust-fmt` — `cargo fmt --all --check` (fast; skips docs-only
    commits). Blocks commits that CI's format gate would reject.
  - `scripts/hooks/pre-commit.d/30-rules-index` — `check_rules_derive.py --write` when the commit
    touches `docs/rules/`, re-staging the regenerated index so the fix lands in the same commit.
  - `scripts/hooks/pre-commit.d/40-doctrine-regen` — the doctrine generator's `--write`, staging only
    what it rewrote. **Warns and never blocks**: it needs a plugin from a private marketplace and a
    token, so a clone with neither has to stay committable. CI's `provenance` job is what reds.
- **pre-push**
  - `scripts/hooks/pre-push.d/10-rust-clippy` — `cargo clippy --workspace --all-targets -- -D
    warnings`. Runs at the push boundary (not every commit) so the compile cost is paid once;
    skips pushes that touch no Rust.

`--no-verify` bypasses the whole set for deliberate exceptions. They are a local pre-flight —
**CI is the real gate**; skipping setup just means you find out at CI instead of at commit.

**None of them is a substitute for CI, and the reason is structural rather than a list of
shortcomings.** *Every* check here reads your **working tree**; CI reads **what you committed**.
Those are the same thing right up until they are not — `git add -p`, a partial `git add`, an
untracked scratch file — and where they differ, any of these checks can green a commit CI then
reds. The sharp case is `20-rust-fmt`: `cargo fmt --all --check` is byte-for-byte CI's command and
it *still* reads from disk, so staging a badly formatted hunk while the file on disk is clean
passes here and fails the format gate. In the other direction, an untracked scratch file can make
`10-rules-refs` block a commit that has nothing to do with it.

Two checks also differ from CI in the command itself. `30-rules-index` runs `--write` where CI runs
`--check`, which is the whole point of it. `10-rust-clippy` omits CI's `--features
reuben-core/bench`, so a lint that only fires in a `[[bench]]` target passes here and reds there;
the file says so in its header.

### Adding a check

Drop an executable file in `scripts/hooks/pre-commit.d/` or `scripts/hooks/pre-push.d/` and re-run
`./scripts/install-hooks.sh` if the tree lost the executable bit. There is nothing to register and
nothing to displace — which is the reason for the indirection, because the alternative is a second
hooks directory that silently disables the first.

What a check can rely on, and what it owes:

- **Read-only checks take the low numbers; a check that writes takes a high one.** `30-rules-index`
  and `40-doctrine-regen` regenerate and re-stage files, and a read-only check failing after either
  would leave you a modified, staged file you never touched and were never told about.
- **A check is handed the hook's own arguments and a verbatim replay of the hook's stdin**, so
  every pre-push check sees the same pushed refs. It runs from the working-tree root and may stage
  files.
- **An entry that cannot run is an error, not a skip.** Not executable, a broken symlink, or a name
  starting with `.` that the shell's glob cannot see — each would be a check that never runs, and a
  check that never runs is indistinguishable from one that passed. That is the whole bug this
  arrangement exists to prevent, at a smaller scale, so `dispatch` refuses and names the file.
  `./scripts/install-hooks.sh` repairs a lost bit; retiring a check is deleting the file, where
  review can see it; skipping one run is `--no-verify`. **The one thing that is skipped** is an
  editor's `<name>~` backup, which inherits the executable bit and would otherwise run as a stale
  duplicate of the check it shadows.
- **Keep the prefix two digits wide.** Order is the shell's collation order, not numeric, so a
  `100-` check would sort *before* `20-`.
- **A new hook kind needs a stub beside its registry.** `scripts/hooks/pre-commit.d/` is reached only
  because `scripts/hooks/pre-commit` exists — git looks a hook up by its exact filename and nothing
  else. Copy either existing stub and change the name it passes. `install-hooks.sh` refuses a
  registry with no stub rather than listing checks git will never call.
- **Do not rename the stubs, and do not move this directory.** `brain`'s `bootstrap.sh` decides
  whether a repo has hooks by looking for an executable regular file *named after a git hook event*
  directly inside `scripts/hooks/`. `pre-commit` and `pre-push` are what it finds; `dispatch` is not a
  hook name and would not satisfy it. Rename either stub, or move the set, and every bootstrapped
  clone silently goes back to running no hooks at all.

## Toolchain

The Rust version is pinned in [`rust-toolchain.toml`](./rust-toolchain.toml). rustup
auto-installs and uses it the first time you run any `cargo` command in the repo — you
don't pick a toolchain. Because local and CI run the *same* version, a given fmt or clippy command
gives the same verdict in both places — which is what makes the hooks worth trusting. It is the
*commands* that differ where they differ, as above, never the compiler.

**`python3` is optional but wanted.** The pre-commit checks that are not `cargo` are Python.
Without `python3` on `PATH` each prints a warning and steps aside rather than blocking your commits,
because CI runs them regardless. What you lose is their regeneration: commit a `docs/rules/` change
without it and CI's `--check` reds the build, and a doctrine artifact you should have regenerated
stays as it was.

### The bare-metal target

[`rust-toolchain.toml`](./rust-toolchain.toml) also pins **`thumbv7em-none-eabihf`** — bare-metal
Cortex-M7, no OS and therefore no `std` at all, because rustup ships none for any `*-none-*` triple.
rustup installs it alongside the channel, so there is no `rustup target add` to remember.
`reuben-core` is the crate that has to keep building for it; nothing above it in the workspace does.

**The gate passes.** The port is finished: `reuben-core` and `reuben-contract` both carry the
`no_std` attribute, every dependency reaching **this target** arrives with its default `std`
features off, and the transcendental `f32` math that only `std` defines now reaches the target
through `num_traits::Float`. CI's `bare-metal build (thumbv7em-none-eabihf)` job is nonetheless still
**not** one of `ci-passed`'s dependencies, so it blocks no merge — wiring it in is a separate
change that has not been made yet, and until it is, a red run here is easy to miss.

The job is two commands, and they are the two to run locally against it:

```sh
cargo build  -p reuben-core --target thumbv7em-none-eabihf --release
cargo clippy -p reuben-core --target thumbv7em-none-eabihf --release -- -D warnings
```

A `use std::…` anywhere in `crates/reuben-core/` now fails both, and no other check in the repo can
see that. [`.github/workflows/ci.yml`](./.github/workflows/ci.yml) carries the rest: how to read a
red run, and what the job's reported `.text` number does and does not measure — it never fails on
it.

**Two things about the attribute and the math, because neither reads off the source.**

`reuben-core` is `#![cfg_attr(not(test), no_std)]` rather than a bare `#![no_std]`, and there is no
`std` *feature* anywhere — nothing is additive and no command line can forget it. The `not(test)`
covers exactly one thing: rustc links `std` into the lib **test** target while leaving the *core*
prelude in place, so without it every `#[cfg(test)] mod tests` in the crate would owe explicit
`alloc` imports for `String`/`Vec`/`format!` — about 380 of them, in code the shipped rlib does not
contain. The lib target itself builds `no_std` on the host too, which is what makes an ordinary
`cargo clippy --workspace --all-targets` catch a stray `use std::…` in production code on any
machine.

`num-traits`' `Float` backend is chosen **by target** in
[`crates/reuben-core/Cargo.toml`](./crates/reuben-core/Cargo.toml) — `libm` for `target_os = "none"`,
`std` everywhere else — and that file carries the measurements behind the choice. Two consequences
worth knowing before reading a number off either side:

- The host test run proves the crate **compiles** with no `std` to reach, and it proves the host's
  numerics. It does not prove the target's: the bare-metal build is the only one that touches
  `libm` at all, and it cannot run.
- On `thumbv7em-none-eabihf`, `libm` ships no ARM path, so `sqrt` lowers to a software routine
  rather than the FPU's `VSQRT.F32`, and `floor`/`ceil`/`round`/`trunc` to software rather than
  `VRINT*`. On x86-64 and aarch64 `libm` does reach the hardware instruction. That gap is
  upstream's, and nobody has run the target yet.

### Bumping the Rust version

The pinned version and the MSRV are kept **in lockstep** (see
[web-product-process](./docs/rules/web-product-process.md)). To move to a new Rust:

1. `channel` in `rust-toolchain.toml`
2. `rust-version` in `Cargo.toml` `[workspace.package]` — set to the **same** version

Two spots, one conceptual change. The `lockstep` CI job fails the build if they don't match,
so a forgotten second edit is caught immediately. CI then verifies the new floor for free (it builds on
the pinned toolchain, which equals the MSRV).

## Branching & release flow

**The branch model is [`.ii/repo.toml`](./.ii/repo.toml)'s `[branches]` table**, and it is not
restated here. Open every PR against the integration branch it names; the release branch advances
only by the workflow at its `promotion_source`, dispatched by hand from the Actions tab. Prose that
names a branch instead of reading it there goes stale the day the table changes.

Point your clone's `origin/HEAD` at the default branch once, or `origin/HEAD` and anything built on
it resolve against whatever the clone was created against:

```sh
git remote set-head origin -a
```
