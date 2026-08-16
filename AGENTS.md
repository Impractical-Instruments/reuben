# reuben

A configurable musical instrument built from composable **Operators** — small
single-purpose DSP units patched into playable Instruments and Rigs. OSC is the
lingua franca, in and out.

**Stack:** Rust workspace (Cargo). Core: `reuben-core` (the portable engine + its C-ABI embed
surface); the window every consumer goes through: `reuben-api`; binary: `reuben-native`; MCP
sidecar: `reuben-mcp`. This repo is the **SDK** — the
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

One-time setup, unless `brain`'s `bootstrap.sh` already did it: `./scripts/install-hooks.sh` —
points `core.hooksPath` at [`scripts/hooks/`](scripts/hooks), where
[`scripts/hooks/dispatch`](scripts/hooks/dispatch) chains every check registered under
`scripts/hooks/pre-commit.d/` (rules ref-linter, ADR-number guard, `cargo fmt`, rules-index regen,
doctrine regen) and `scripts/hooks/pre-push.d/` (`cargo clippy`). Both routes set the same value.
See [CONTRIBUTING.md](CONTRIBUTING.md).

## Non-negotiable invariants (every code change)

Determinism · RT-safe Render (`process` never allocates/locks/blocks) · OSC-only
core · single-writer Coordinator. Details + enforcing tests:
[authoring.md](docs/agents/authoring.md#invariants-you-must-not-break).

## Comments (every code change)

**Rationale is a rule; mechanics is a comment; restating the code is neither.** Before writing a
comment, decide which of the three it is:

- **Rationale** — argues a position, explains a tradeoff, or states a constraint that binds beyond
  this file → it belongs in a `docs/rules/` topic, and the comment becomes `// see rules: <topic>`.
- **Mechanics** — a local fact the code cannot state itself (`SAFETY:`, an invariant a caller must
  uphold, why a constant is *this* number, a non-obvious step, the signature-level `///`) → keep it.
- **Restatement** — anything `hover`, `goToDefinition`, `findReferences`, or a grep would answer →
  don't write it. It is a second copy that can go stale while the build stays green.

Never cite an issue or ADR number in a comment; provenance lives in the rationale file. Guarded
across every crate by `scripts/check_rules_refs.py`. Full rule:
[Code as a grounding surface](docs/rules/code-as-grounding.md).

## Rule conflicts (every change)

**If your output contradicts a live rule, say so — never override one silently.** Name the rule and
why it is worth reopening, in the change itself. The rules under [`docs/rules/`](docs/rules/README.md)
state the now, so contradicting one without saying so leaves the corpus asserting something the code
no longer does, and nothing notices.

Reopening a settled rule is a new ADR under [`docs/adr/`](docs/adr/README.md), the iteration surface a
later `absorb-adrs` sweep folds back into the rules. An ADR that overturns a rule also has to mark
that rule in the same change — that protocol, and the guard behind it, are in
[docs/adr/README.md](docs/adr/README.md).

## Language

Use the project's exact terms (Operator, Instrument, Rig, Plan, Swap, Voice…).
The [rules index](docs/rules/README.md) carries the glossary — don't drift to synonyms its [Avoid these synonyms](docs/rules/README.md#avoid-these-synonyms) list calls out.

## Repo map

Eight crates. The engine is two of them — `reuben-core` (render) and `reuben-document` (authoring)
— and together they are ~35k lines; enter through the module that owns the concept, not a search.

| Crate | Owns |
| --- | --- |
| `reuben-core` | The render half of the engine: the portable, OS-free path from a `Plan` to a block of audio. No OS dependencies, and it names no crate above it. |
| `reuben-document` | The authoring half: the instrument format and its loader, the edit verbs, the projections, and the off-thread Coordinator. Sits above `reuben-core` and imports upward. |
| `reuben-api` | The one window every consumer goes through: the authoring verbs, the engine verbs (swap/control/status/diagnostics) and both ends of the structure channel, plus the tool roster and its advertised prose. Two feature halves — `authoring` (off-thread, serialized) and `render` (the Coordinator + RenderSlot pair a host drives per block) — plus the resource seam both halves call and the default-off `fs-resolver` reference implementation of it. |
| `reuben-native` | The removable native layer: cpal audio + input, OSC/UDP decode, the `reuben` CLI. Reaches the engine through nothing but the window, render half included. |
| `reuben-mcp` | The per-conversation MCP stdio sidecar: the roster, the stdio transport, the loopback socket. Reaches `reuben-core` through nothing but the window. The only member allowed an async runtime (rmcp + tokio). |
| `reuben-contract` | The single source of an Operator's port/constant contract, shared by the macro and scaffold. |
| `reuben-macros` | `operator_contract!` — emits the index consts *and* the `Descriptor` from one declaration. |
| `reuben-guide` | Docs tooling, not engine code: slices `docs/agents/authoring.md` into its per-delivery-lane cuts. Depends on nothing in the workspace. |

Inside `reuben-core` (the full version is the `crates/reuben-core/src/lib.rs` doc comment):
data model `signal` (audio-rate) + `message` (OSC-shaped) · authoring `operator` +
`descriptor` · composition `graph` → `plan` (Instantiate) → `render` (per-block) · musical
`vocab` + `tuning` · the Operator set in `operators/`.

Most-wanted specifics: Swap lifecycle `coordinator/` · instrument JSON `format/` · the agent's
whole view of a document `projection.rs` · type name → constructor `registry.rs`.

## Code navigation

Prefer LSP over Grep/Glob for code navigation:

- goToDefinition / goToImplementation to locate source
- findReferences before any rename or signature change — enumerate all call sites first
- workspaceSymbol / documentSymbol to find definitions
- hover for type info without reading the file
- check diagnostics after every edit; fix type errors before moving on

Use Grep only for non-code text: comments, string literals, config values.
**Never use Grep to find a function or type definition.**

These files punish a whole-file Read — `documentSymbol` first, then read only the range you need:
`crates/reuben-document/src/format/mod.rs` (4.8k lines) · `crates/reuben-mcp/src/lib.rs` (3.3k) ·
`crates/reuben-document/src/projection.rs` (2.5k) · `crates/reuben-native/src/audio.rs` (1.5k) ·
`crates/reuben-core/src/plan.rs` (1.5k).

Search is pre-scoped by [`.ignore`](.ignore) — build output, `.git`, caches, binary fixtures.
Don't bypass it with `--no-ignore`; nothing it hides is a source of truth.

Both of the above are rules, not preferences — see
[Code as a grounding surface](docs/rules/code-as-grounding.md).

## Guides

- **[Authoring](docs/agents/authoring.md)** — the instrument-authoring guide: JSON format, type system + wiring, addressing, the authoring loop.
- **[Operator dev](docs/agents/operator-dev.md)** — operator trait, descriptor macro, adding an operator, RT-safety rules.
- **[Intent vocabulary](docs/agents/vocabulary.md)** — the word→move table turning intent language ("warmer", "busier", "sadder") into parameter moves. Generated from `vocabulary.json`; also served as `reuben://guide/vocabulary`.
- **[Domain docs](docs/agents/domain.md)** — the now-state architecture is the [rules index](docs/rules/README.md) → topic → rule → rationale; read the index + the relevant topic doc before exploring. `docs/adr/` is the live iteration surface a human periodically folds into rules with the `absorb-adrs` skill.
- **[Canonical sources](docs/agents/canonical-sources.md)** — the registry of what this repo copies in from elsewhere, and how each copy is refreshed. The rule those entries obey is the company-doctrine region at the end of the [rules index](docs/rules/README.md).
- **[Agent-surface eval](eval/README.md)** — what authoring costs a model (grounding tokens, repair rounds, freehand JSON). Gated in CI; run `cd eval && python3 -m reuben_eval.gate` after changing a tool description, the `instructions`, or `docs/agents/`.
- **[Benchmarks](crates/reuben-document/benches/README.md)** — two workloads, each in a local wall-clock and a CI instruction-count layer: **render** (`render_block`, gated on absolute cost against the base ref) and **construct** (load + instantiate, swept across node counts and gated on how it *scales*, at no baseline). Bench case ids are matched by name across commits — renaming one drops it from its gate and orphans its history.
- **[Issue tracker](docs/agents/issue-tracker.md)** — GitHub Issues via `gh`; external PRs are not a triage surface.
- **[Triage labels](docs/agents/issue-tracker.md)**, under `## Labels` — the state roles and the charting family, which mean the same thing in every repo, plus the labels this repo adds for itself.
- **[CONTRIBUTING.md](CONTRIBUTING.md)** · **[Rules index](docs/rules/README.md)** · **[Live ADRs](docs/adr/README.md)** (the iteration surface) · **[`.ii/repo.toml`](.ii/repo.toml)** — this repo as data: the branch model, the doc system, and every copy it declares. Read it rather than asking prose.
