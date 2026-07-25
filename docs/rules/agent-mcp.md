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
behind `reuben scaffold-operator` / `new_instrument`, so the author is left only the creative half and
starts from a guaranteed-valid or compiling frame.

Conversational authoring rides an **MCP sidecar**: a disposable per-conversation stdio process the
client spawns, hosting the pure tools in-process and forwarding the engine tools to a long-lived,
**user-owned** `reuben play` — so the sound survives conversation death and the shim never spawns or
kills the engine. rmcp and tokio are fenced in that one crate; the rest of the workspace stays
std-only. The tool surface is a fixed roster of three kinds: pure tools and engine-free **document
verbs**, both always available, plus engine tools that fail fast with "start `reuben play`" when it is
absent — all returning structured `Report`/`Diag` results under a strict error-layer discipline —
**a failed validation is a successful call**, and `isError` is reserved for the tool that could not do
its job. The edit contract is that closed **document vocabulary**: path-addressed, stateless verbs
that each apply one surgical edit to a named `source`, re-validate the *whole* document through the
loader, and write only if it is valid — never the whole document in and out, which cost a model every
byte it was not changing. The read side matches: the agent's whole view is a set of partial
**structural projections** (index, node zoom carrying reverse edges, pipes, resources), lossless only
in aggregate, so a turn pays for the nodes it touches rather than the file. `send` stays ephemeral
audition (clobbered at the next swap) against a document that is durable truth — try-then-commit. **No
reuben-owned bytes ride the agent's context** on any lane: a sample is a filesystem gesture, a document
is named by an opaque source the door's resolver moves. Where a door's clients can race, the door
carries its own optimistic `expect` guard — a content-hash compare it makes before calling in,
answered in its own shape — since core's swap is unguarded last-write-wins.

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
### The mechanical half of authoring is deterministic codegen behind a reuben verb — scaffold-operator, new-instrument — that lands a guaranteed-valid or compiling starting frame, leaving only the creative half.

[why](rationale/agent-mcp/deterministic-scaffolds.md)

<a id="mcp-stdio-sidecar"></a>
### The MCP server is a disposable per-conversation stdio sidecar that hosts the pure tools in-process and forwards engine tools to a long-lived engine, with rmcp and tokio fenced in its own crate.

[why](rationale/agent-mcp/mcp-stdio-sidecar.md)

<a id="user-owned-engine"></a>
### The user owns the engine: the sidecar never spawns or kills reuben play, engine-touching tools fail fast with actionable guidance, and multiple clients are tolerated rather than arbitrated.

[why](rationale/agent-mcp/user-owned-engine.md)

<a id="expect-guard-is-a-door-concern"></a>
### The optimistic-concurrency expect guard belongs to each door, not to core: a core swap is unguarded last-write-wins, and a door with concurrent clients compares the content hash its client holds against the installed one and answers in its own shape before calling in.

[why](rationale/agent-mcp/expect-guard-is-a-door-concern.md)

<a id="contract-holds-what-core-produces"></a>
### A serde type belongs to the contract if core itself produces it and to a door's wire module if it exists only because that door exists — the test being whether the type would still mean anything with the door deleted.

[why](rationale/agent-mcp/contract-holds-what-core-produces.md)

<a id="structure-channel-is-loopback-only"></a>
### The structure channel binds loopback only, because structure edits are strictly more powerful than OSC control — and its one default address is shared with the wire types both ends serialize so the server and client can never drift apart.

[why](rationale/agent-mcp/structure-channel-is-loopback-only.md)

<a id="portable-tool-contracts"></a>
### The tool contract types and introspection live OS-free in reuben-core, so every door — native CLI, MCP sidecar, web in-page layer, web proxy — generates its schemas from that one source and no verb means different things behind different doors.

[why](rationale/agent-mcp/portable-tool-contracts.md)

<a id="document-verbs"></a>
### The agent authors through a closed vocabulary of path-addressed, stateless, engine-free document verbs, each applying one surgical edit to the named source, re-validating the whole document through the loader, and writing only if it is valid.

[why](rationale/agent-mcp/document-verbs.md)

<a id="document-projection"></a>
### The agent never loads a reuben-owned document into its context: its whole view is a set of partial structural projections — index, node zoom with reverse edges, pipes, resources — single-sourced in reuben-core and lossless only in aggregate.

[why](rationale/agent-mcp/document-projection.md)

<a id="try-then-commit"></a>
### `send` is ephemeral live audition, clobbered at the next swap, and the document is the durable truth — so the authoring loop is try-then-commit.

[why](rationale/agent-mcp/try-then-commit.md)

<a id="tool-surface"></a>
### The MCP tool surface is a fixed roster of three kinds — always-available pure tools, engine-free document verbs, and fail-fast engine tools — returning structured Report/Diag results where a failed validation is a successful call, and shipping resources but no prompts.

[why](rationale/agent-mcp/tool-surface.md)

<a id="no-resource-bytes"></a>
### No reuben-owned bytes ride the agent's context: using a sample is a filesystem gesture the agent performs with its own file tools, a document is moved by the door's resolver behind an opaque source, and in the browser bytes reach the engine only through the staging seam.

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

<a id="grounding-budget-is-relative"></a>
### A grounding projection's size is gated relative to the full view it compresses, never as a flat per-item cap — so the lever on a budget the registry has outgrown is the projection's own compression, not the number of operators.

[why](rationale/agent-mcp/grounding-budget-is-relative.md)

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
- **Document verb** — one member of the closed, format-derived vocabulary an agent authors with: a stateless `(source, …)` mutator that applies one surgical edit, re-validates the whole document, and writes iff valid.
- **Structural projection** — the agent's whole view of a document: partial per view (index, node zoom with reverse edges, pipes, resources), lossless only in aggregate, and single-sourced in reuben-core.
