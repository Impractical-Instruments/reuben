# reuben

A configurable musical instrument built from composable **Operators** — small
single-purpose DSP units patched into playable Instruments and Rigs. OSC is the
lingua franca, in and out.

**Stack:** Rust workspace (Cargo). Core: `reuben-core` (the portable engine + its C-ABI embed
surface); binary: `reuben-native`; MCP sidecar: `reuben-mcp`. This repo is the **SDK** — the
browser player and its chat-authoring agent were extracted to the private `reuben-web` repo,
which consumes this one as a submodule.

## Commands

```sh
cargo test --workspace
cargo fmt --all --check                                # CI format gate
cargo clippy --workspace --all-targets -- -D warnings  # CI lint gate
cargo run -p reuben-native --example gen_library_index # after ANY instrument change
cargo run -p reuben-core --example gen_vocabulary      # after editing docs/agents/vocabulary.json
cargo run -p reuben-native --bin reuben -- describe    # list operators/ports/params
```

One-time setup: `git config core.hooksPath .githooks` (fmt pre-commit, clippy pre-push).

## Non-negotiable invariants (every code change)

Determinism · RT-safe Render (`process` never allocates/locks/blocks) · OSC-only
core · single-writer Coordinator. Details + enforcing tests:
[authoring.md](docs/agents/authoring.md#invariants-you-must-not-break).

## Language

Use the project's exact terms (Operator, Instrument, Rig, Plan, Swap, Voice…).
The [rules index](docs/rules/README.md) carries the glossary — don't drift to synonyms its [Avoid these synonyms](docs/rules/README.md#avoid-these-synonyms) list calls out.

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
- **[Domain docs](docs/agents/domain.md)** — the now-state architecture is the [rules index](docs/rules/README.md) → topic → rule → rationale; read the index + the relevant topic doc before exploring. `docs/adr/` is the live iteration surface a human periodically folds into rules with the `absorb-adrs` skill.
- **[Agent-surface eval](eval/README.md)** — what authoring costs a model (grounding tokens, repair rounds, freehand JSON). Gated in CI; run `cd eval && python3 -m reuben_eval.gate` after changing a tool description, the `instructions`, or `docs/agents/`.
- **[Issue tracker](docs/agents/issue-tracker.md)** — GitHub Issues via `gh`; external PRs are not a triage surface.
- **[Triage labels](docs/agents/triage-labels.md)** — needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix.
- **[CONTRIBUTING.md](CONTRIBUTING.md)** · **[Rules index](docs/rules/README.md)** · **[Live ADRs](docs/adr/README.md)** (the iteration surface)
