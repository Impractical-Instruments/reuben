# reuben

A configurable musical instrument built from composable **Operators** — small
single-purpose DSP units patched into playable Instruments and Rigs. OSC is the
lingua franca, in and out.

**Stack:** Rust workspace (Cargo). Core: `reuben-core` (the portable engine + its C-ABI embed
surface); binary: `reuben-native`; MCP sidecar: `reuben-mcp`. This repo is the **SDK** — the
browser player and its chat-authoring agent were extracted to the private `reuben-web` repo,
which consumes this one as a submodule (ADR-0056).

## Commands

```sh
cargo test --workspace
cargo fmt --all --check                                # CI format gate
cargo clippy --workspace --all-targets -- -D warnings  # CI lint gate
cargo run -p reuben-native --example gen_library_index # after ANY instrument change
cargo run -p reuben-native --bin reuben -- describe    # list operators/ports/params
```

One-time setup: `git config core.hooksPath .githooks` (fmt pre-commit, clippy pre-push).

## Non-negotiable invariants (every code change)

Determinism · RT-safe Render (`process` never allocates/locks/blocks) · OSC-only
core · single-writer Coordinator. Details + enforcing tests:
[authoring.md](docs/agents/authoring.md#invariants-you-must-not-break).

## Language

Use the project's exact terms (Operator, Instrument, Rig, Plan, Swap, Voice…).
[CONTEXT.md](CONTEXT.md) is the glossary — don't drift to synonyms it says to avoid.

## Code navigation
Prefer LSP over Grep/Glob for code navigation:
- goToDefinition / goToImplementation to locate source
- findReferences before any rename or signature change — enumerate all call sites first
- workspaceSymbol / documentSymbol to find definitions
- hover for type info without reading the file
- check diagnostics after every edit; fix type errors before moving on
  Use Grep only for non-code text: comments, string literals, config values.
  Never use Grep to find a function or type definition.

## Guides

- **[Authoring](docs/agents/authoring.md)** — the instrument-authoring guide: JSON format, type system + wiring, addressing, the authoring loop.
- **[Operator dev](docs/agents/operator-dev.md)** — operator trait, descriptor macro, adding an operator, RT-safety rules.
- **[Domain docs](docs/agents/domain.md)** — read CONTEXT.md + relevant ADRs before exploring.
- **[Issue tracker](docs/agents/issue-tracker.md)** — GitHub Issues via `gh`; external PRs are not a triage surface.
- **[Triage labels](docs/agents/triage-labels.md)** — needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix.
- **[CONTRIBUTING.md](CONTRIBUTING.md)** · **[Architecture](docs/ARCHITECTURE.md)** · **[ADRs](docs/adr/)**
