# Agent framework & MCP

> How AI agents author reuben — authorability as a first-class constraint, the introspect/validate loop, the authoring skills, and the MCP sidecar whose tool contracts are one OS-free source behind every door.

## Now

reuben is built to be authored by AI agents, and that is a **first-class design constraint**, not a
bolted-on feature: operators are self-describing, the instrument is one recursive JSON graph, the
library is composed by reference, and a suite of **authoring skills** is a product deliverable. The
constraint pays off through a closed feedback loop the agent can drive without ears or a running
engine. Two pure functions in `reuben_core::introspect` are the whole introspection surface —
**describe** an operator's ports and params from the live registry, and **validate** a drafted
document by running the engine's own load-plus-instantiate path with no audio device opened. That
loader is the **single validation authority**: validate means "does the engine itself accept this?",
so there is no second, drifting schema gate. What the loop cannot catch — that a validated patch is
actually *audible*, that a compiled operator actually *sounds right* — is the skills' job, carried as
moderate semantic guidance. The mechanical, error-prone half of authoring (new-operator boilerplate
across Rust files, the required top-level fields of a fresh document) is **deterministic codegen**
behind `reuben scaffold-operator` / `scaffold_instrument`, so the author is left only the creative
half and starts from a guaranteed-valid or compiling frame.

Conversational authoring rides an **MCP sidecar**: a disposable per-conversation stdio process the
client spawns, hosting the pure tools in-process and forwarding the engine tools to a long-lived,
**user-owned** `reuben play` — so the sound survives conversation death and the shim never spawns or
kills the engine. rmcp and tokio are fenced in that one crate; the rest of the workspace stays
std-only. The tool surface is a fixed roster: pure tools always available, engine tools that fail
fast with "start `reuben play`" when it is absent, all returning structured `Report`/`Diag` results
under a strict error-layer discipline — **a failed validation is a successful call**, and `isError`
is reserved for the tool that could not do its job. The edit contract is the **whole document in, a
report out**: no add-node/rewire surface exists, `send` is ephemeral audition (clobbered at the next
swap), and the document is durable truth (try-then-commit). No bytes cross the wire — using a sample
is a filesystem gesture.

The load-bearing invariant under all of this is **one source, many doors**: the contract types and
introspection live OS-free in `reuben-core`, so the native CLI, the MCP sidecar, the web in-page tool
layer, and the web proxy all generate their schemas from that one source and no verb means different
things behind different doors. MCP is one door, not the contract — web parity ports the contracts,
not the protocol. Grounding is **single-sourced** the same way: normative prose lives once (the
authoring guide, the intent vocabulary, the library index), and code, skills, and server
`instructions` **gist-and-point** at it rather than restating it. Grounding also splits by
**direction**: input handling (reading "warmer / busier / sadder" as parameter moves, via one
curated registry-keyed word→move table) is shared base sauce delivered to every lane, while output
filtering (the sound-not-machine persona) is host-owned flavor. There is deliberately **no instrument
JSON Schema** in that grounding — an agent grounds on prose rules, ports, and the validator loop —
and the conversational loop is proven by a fixed menu of tests, from live-channel
integration tests down to scripted human rituals for the perceptual judgments automation cannot reach.

## Rules

<a id="ai-authorability"></a>
### AI-agent authorability is a first-class design constraint, served by self-describing operators, one recursive graph model, an agent-native JSON format, a referenced library, and a suite of authoring skills.

[why](rationale/agent-mcp/ai-authorability.md)

<a id="introspection-surface"></a>
### Introspection is thin pure functions over the static registry and the real load path — describe an operator, validate a document — with no query into a running engine.

[why](rationale/agent-mcp/introspection-surface.md)

<a id="loader-single-authority"></a>
### The engine's own load-plus-instantiate path is the single validation authority, and validate runs exactly it — there is no second schema-validation gate.

[why](rationale/agent-mcp/loader-single-authority.md)

<a id="authoring-skills"></a>
### Each authoring audience has a skill that closes its own introspect-or-scaffold, draft, validate-or-test, report loop, and carries the semantic judgement the validator cannot (validate-pass is not audible).

[why](rationale/agent-mcp/authoring-skills.md)

<a id="deterministic-scaffolds"></a>
### The mechanical half of authoring is deterministic codegen behind a reuben verb — scaffold-operator, scaffold-instrument — that hands the author a guaranteed-valid or compiling starting frame, leaving only the creative half.

[why](rationale/agent-mcp/deterministic-scaffolds.md)

<a id="mcp-stdio-sidecar"></a>
### The MCP server is a disposable per-conversation stdio sidecar that hosts the pure tools in-process and forwards engine tools to a long-lived engine, with rmcp and tokio fenced in its own crate.

[why](rationale/agent-mcp/mcp-stdio-sidecar.md)

<a id="user-owned-engine"></a>
### The user owns the engine: the sidecar never spawns or kills reuben play, engine-touching tools fail fast with actionable guidance, and multiple clients are tolerated rather than arbitrated.

[why](rationale/agent-mcp/user-owned-engine.md)

<a id="portable-tool-contracts"></a>
### The tool contract types and introspection live OS-free in reuben-core, so every door — native CLI, MCP sidecar, web in-page layer, web proxy — generates its schemas from that one source and no verb means different things behind different doors.

[why](rationale/agent-mcp/portable-tool-contracts.md)

<a id="whole-document-edit"></a>
### A conversational edit is the whole instrument document in and a report out — with no incremental edit-command surface — where send is ephemeral audition and the document is the durable truth (try-then-commit).

[why](rationale/agent-mcp/whole-document-edit.md)

<a id="tool-surface"></a>
### The MCP tool surface is a fixed roster split into always-available pure tools and fail-fast engine tools, returning structured Report/Diag results where a failed validation is a successful call, and shipping resources but no prompts.

[why](rationale/agent-mcp/tool-surface.md)

<a id="no-resource-bytes"></a>
### No tool accepts resource bytes: using a sample is a filesystem gesture the agent performs with its own file tools, and in the browser bytes reach the engine only through the staging seam.

[why](rationale/agent-mcp/no-resource-bytes.md)

<a id="grounding-single-source"></a>
### Authoring grounding is single-sourced — normative prose lives once in the authoring guide and the skills, CLI, and MCP server point at it (gist-and-point) — while every door descends to the same introspect and loader so facts cannot drift.

[why](rationale/agent-mcp/grounding-single-source.md)

<a id="intent-vocabulary"></a>
### Musical intent language grounds in one curated, registry-keyed word-to-move table delivered in-prompt and instrument-blind, joined to the concrete document in the agent's context, and kept referentially fresh by CI and musically fresh by evals.

[why](rationale/agent-mcp/intent-vocabulary.md)

<a id="cross-lane-grounding"></a>
### Grounding splits by direction, not persona: input handling (reading intent as moves) is shared base sauce delivered to every lane, while output filtering (the sound-not-machine persona) is host-owned flavor.

[why](rationale/agent-mcp/cross-lane-grounding.md)

<a id="grounding-not-schema"></a>
### There is no instrument JSON Schema for agent grounding; an agent grounds on prose rules, operator ports, and the validator loop, and registry truth is guarded by same-commit native-versus-wasm describe parity.

[why](rationale/agent-mcp/grounding-not-schema.md)

<a id="conversational-loop-verification"></a>
### The conversational authoring loop is verified by a fixed menu — live-channel integration tests, Coordinator-direct behavioral swap checks, allocation-counting for RT-safety, and scripted human rituals where automation cannot reach.

[why](rationale/agent-mcp/conversational-loop-verification.md)

## Terms

- **Sidecar** — the disposable per-conversation MCP stdio process the client spawns: pure tools in-process, engine tools forwarded to the user-owned engine.
- **Door** — one surface over the OS-free contract types (native CLI, MCP sidecar, web in-page layer, web proxy); no verb means different things behind different doors.
- **Gist-and-point** — the anti-drift posture for prose that must live in code: carry the one-breath gist and point at the single canonical doc, never restate it.
- **Intent vocabulary** — the one curated, registry-keyed word→move table that grounds musical/mood words (warmer, busier, sadder) as operator-type parameter moves.
- **Input handling** — interpreting musical, mood, or abstract language as patching moves; the shared base grounding identical in every lane.
- **Output filter** — the host-owned persona: what the person is shown (sound-not-machine subject, hidden diagnostics, register), maximal on web and absent at skills/MCP.
- **Delivery lane** — a grounding consumer (repo skills, MCP clients, web chat), each reducing to transport bindings plus host furniture plus the shared base sauce, fed by push or pull.
