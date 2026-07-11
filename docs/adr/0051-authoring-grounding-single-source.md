# ADR-0051: One source of truth for authoring grounding — skills, CLI, and MCP server

## Status

Accepted (2026-07-11). The skills↔server single-sourcing decision of the reuben MCP server
effort — wayfinder ticket [MCP/H (#278)](https://github.com/Impractical-Instruments/reuben/issues/278)
on map [#270](https://github.com/Impractical-Instruments/reuben/issues/270), resolving
[#220](https://github.com/Impractical-Instruments/reuben/issues/220)'s open question 3
("what is the single source of truth for how-to-author prompt material?"). **Rides on**
[ADR-0020](0020-introspection-and-patcher-skill.md) (describe/validate as pure library
functions), [ADR-0044](0044-mcp-stdio-sidecar.md) (stdio sidecar descending to
`reuben_core::introspect`), and [ADR-0048](0048-mcp-tool-surface-and-contracts.md) §7 (the
fixed resource surface: `reuben://schema/instrument` + `reuben://guide/authoring`, server
`instructions`, no prompts). **Lands the guide obligations** handed here by
[ADR-0049](0049-no-resource-bytes-over-mcp.md) §4 (the sample workflow) and
[ADR-0050](0050-swap-sonic-rudeness-ramp.md) (the two swap rules of thumb). Feeds the
implementation epic (MCP/J).

## Context

- **Code-level drift is structurally impossible; prose drift is the whole battle.** The CLI
  (`reuben describe/validate`, ADR-0020) and the MCP sidecar (ADR-0044 §3) descend to the same
  pure functions in `reuben_core::introspect` and the same loader — the two surfaces cannot
  disagree about facts. What *can* drift is prose, and it currently lives in three going on
  four places: `docs/agents/authoring.md`, the contract-inlining skills (`patcher`,
  `create-operator`, `control-surface` — the trio `sync-docs` exists to sweep), and, once
  `reuben-mcp` ships, the server's `instructions` paragraph plus eight tool-description
  strings (which ADR-0048 requires to carry try-then-commit, send-is-ephemeral, and
  start-`reuben play` guidance).
- **The two verb surfaces don't fully overlap.** `scaffold-operator`, `gen_schema`, the
  TouchOSC emitter, and `cargo test` are CLI/cargo-only — `create-operator` and
  `control-surface` cannot fully migrate. Conversely `send`, `swap`, `engine_status`,
  `get_current_instrument`, and `get_diagnostics` are MCP-only — the CLI has **no
  engine-attached verbs at all**. Only `describe`/`validate` exist on both.
- **`cargo run` tracks source; a built sidecar can be stale.** For repo-dev skills that *edit
  operator code*, a prebuilt sidecar binary answers `describe` from yesterday's contract;
  `cargo run` recompiles. The MVP persona (a dev with a checkout, ADR-0044) means both
  surfaces coexist in one session: a Claude Code conversation in the repo can hold the
  `patcher` skill *and* the connected MCP server.
- **`docs/agents/authoring.md` is a mixed-audience doc.** Roughly half is the instrument
  format, wiring rules, and addressing — exactly what a conversational authoring agent needs;
  the other half is the Rust `Operator` trait, the `operator_contract!` macro, `OpDriver`
  testing, and the add-an-operator steps — essential to `create-operator`, unactionable for
  an MCP client driving tools. ADR-0048 §7 named this file as the backing of
  `reuben://guide/authoring` and left *what content* to this decision.

## Decision

### 1. Transport: partition by verb class — no verb is taught two ways

The repo skills keep `cargo run … describe/validate --json` as their canonical grounding: it
works in a bare checkout with no MCP client configuration and can never be stale. The
`patcher` skill gains a **live-loop section**: when the reuben MCP server is connected, drive
audition and installation with its engine verbs (`send`, `swap`, `engine_status`,
`get_diagnostics`) — verbs that have no CLI equivalent. Each verb has exactly one taught
transport; the skills and the server compose rather than compete.

**Considered and rejected:** migrating `describe`/`validate` to MCP tools when connected
(imports the stale-sidecar drift risk into the workflows most likely to be editing operator
source, and makes every skill's grounding conditional); strict CLI-only with skills ignoring
the server (the MVP persona holds both — a session would shell out awkwardly to audition
while `send`/`swap` sit connected and unused).

### 2. The guide: `docs/agents/authoring.md` refocuses as the instrument-authoring guide

The file backing `reuben://guide/authoring` stays `docs/agents/authoring.md` (ADR-0048 §7's
mapping unchanged), and its content refocuses on the authoring agent — either persona,
skill-driven or MCP-driven:

- the recursive model; the `Arg`/form type system and wiring rules; the instrument format
  contract (`nodes`/`inputs`/`config`/`interface` pipes/`resources`); addressing and OSC;
- the authoring loop's semantics: the document is durable truth, `send` is ephemeral
  audition, try-then-commit (ADR-0045 §5);
- the **sample workflow** required by ADR-0049 §4: placement convention (sample next to the
  instrument, `resources` entry by logical id, relative path), the agent-writes-bytes-itself
  pattern, and the degrade behavior (missing = silence + localized warning);
- the **two swap rules of thumb** required by ADR-0050: a swap ducks the output for ~20ms;
  a note-off racing a swap can hang a note — re-send the off.

The Rust operator-development content — the `Operator` trait, `operator_contract!`,
descriptor/registration, `OpDriver` testing, the add-an-operator steps, the RT-safety
invariants as they bind operator authors — **extracts to a new
`docs/agents/operator-dev.md`**, which points back at the guide's type-system section rather
than restating it (the `Arg`/forms material serves both audiences and lives once, in the
guide).

**Considered and rejected:** serving `authoring.md` as-is and appending (MCP clients pay
~600 lines including Rust internals they cannot act on; the audience stays muddled); a fresh
guide file with `authoring.md` untouched (mints a brand-new format-prose drift pair — the
disease this ticket exists to cure — and quietly falsifies ADR-0048 §7's mapping).

### 3. Skills thin to pointers: normative prose lives once

The contract-inlining skills keep their **workflow** (steps, canonical recipes, scope tables,
report formats) and their worked examples as pedagogy, and drop their *normative* contract
prose in favor of pointers at the canonical doc:

- `patcher` → the guide (its format-rules section is the guide's content);
- `create-operator` → `docs/agents/operator-dev.md` (where its contract content moves);
- `control-surface` already grounds on `surfaces/surface.schema.json` + ADR-0043 — no new
  home needed; its pointer posture is unchanged.

`sync-docs`'s skill sweep narrows from "re-verify three inlined contract copies" to
"workflow drift only".

**Considered and rejected:** keeping skills self-contained with `sync-docs` refereeing
(permanently accepts three copies of the format rules held together by a manual sweep);
thinning `patcher` only (leaves `create-operator`'s inline copy adrift from the very doc its
content moves into).

### 4. Server prose: gist-and-point; resources read from the checkout at request time

The `reuben-mcp` prose surfaces — the `instructions` field and the tool descriptions —
**never restate the contract**: they carry the one-breath gist and point at
`reuben://guide/authoring`, which is available in-band to every connected client. This is the
**gist-and-point** posture (glossary term): duplication is a drift pair; a pointer can't
drift. `sync-docs` grows the `reuben-mcp` prose strings as a sweep target alongside its
existing scope (schema regeneration, the guide, the skills' workflow sections).

The sidecar serves `reuben://schema/instrument` and `reuben://guide/authoring` by **reading
the files from the checkout at request time** — no `include_str!` — so a binary built
yesterday cannot serve yesterday's guide. (The MVP persona runs the sidecar from the repo,
ADR-0044; there is no deployment where the files are absent.)

**Considered and rejected:** sweep-only with unconstrained server prose (four prose copies
held together by nothing but the sweep); mechanical assembly of descriptions from guide
fragments at build time (heavy machinery for ~9 strings, couples the crate build to doc
layout, and build-time embedding recreates the stale-binary problem the runtime read
eliminates).

### 5. The epic obligation: one early-M1 content pass

The implementation epic (MCP/J) carries **one content-pass ticket, staged early in M1**:
refocus `authoring.md` per §2, extract `operator-dev.md`, thin the skills per §3, extend
`sync-docs`'s scope per §4. The server's resources ticket depends on it — ADR-0049 §4's
required content must exist before `reuben://guide/authoring` ships. This ADR is the spec;
the pass is deliberately *not* performed on the map (plan, don't do).

## Consequences

- Prose single-sourcing is achieved **by construction** where a canonical doc exists (one
  guide, one operator-dev doc, skills point), **by posture** where prose must live in code
  (gist-and-point for server strings), and by **manual sweep only as backstop**
  (`sync-docs`'s narrowed skill sweep + the new `reuben-mcp` target).
- `docs/agents/authoring.md`'s audience becomes *the authoring agent* — the same doc grounds
  a repo skill session and an MCP client; `docs/agents/operator-dev.md` becomes the builder
  doc.
- `sync-docs`'s scope table changes: add the `reuben-mcp` prose strings and
  `docs/agents/operator-dev.md`; the three-skill sweep narrows to workflow drift.
- ADR-0048 §7's URI→file mapping stands unamended; ADR-0049 §4's and ADR-0050's guide
  obligations have their landing site.
- The epic gains one early-M1 documentation ticket and a dependency edge from the resources
  ticket onto it.
- Glossary: **Gist-and-point** (CONTEXT.md).
