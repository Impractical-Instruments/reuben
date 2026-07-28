# Agent framework & MCP

> How AI agents author reuben — authorability as a first-class constraint, the introspect/validate loop, the authoring skills, and the MCP sidecar whose tool contracts are one OS-free source behind every door.

## Now

reuben is built to be authored by AI agents, and that is a **first-class design constraint**, not a
bolted-on feature: operators are self-describing, the instrument is one recursive JSON graph, the
library is composed by reference, and a suite of **authoring skills** is a product deliverable. The
constraint pays off through a closed feedback loop the agent can drive without ears or a running
engine. The pure contracts behind the `reuben-api` window are the whole introspection surface —
**describe** an operator's ports and params from the live registry, **describe** a drafted document's
boundary and structure, and **validate** it by running the engine's own load-plus-instantiate path
with no audio device opened. That loader is the **single validation authority**, so there is no
second, drifting schema gate; what it cannot catch — that a validated patch is actually *audible* —
is the skills' job, carried as moderate semantic guidance. The mechanical half of authoring is
**deterministic codegen** behind `reuben scaffold-operator` / `reuben new-instrument`, leaving the
author only the creative half.

Conversational authoring rides an **MCP sidecar**: a disposable per-conversation stdio process the
client spawns, hosting the pure tools and the engine-free **document verbs** in-process and
forwarding the engine tools to a long-lived, **user-owned** `reuben play`, so the sound survives
conversation death and the shim never spawns or kills the engine. rmcp and tokio are fenced in that
one crate; the rest of the workspace stays std-only. The edit contract is a closed, path-addressed
**document vocabulary** and the read side is a set of partial **structural projections**, so no
reuben-owned bytes ride the agent's context — on every lane now, the web door included: it serves the
whole document vocabulary source-addressed, naming documents into a store the host owns rather than
carrying them. Inputs live in **one address space** rather than one per kind of slot — an interface input
pipe is the node it mints, reached by every verb that addresses an input — while the *operations*
over that space are not uniform, so a verb that cannot act on an address refuses by naming what the
address is rather than denying it exists. What a verb echoes back is decided by whether it wrote
anything the caller did not name, and every argument surface is closed, so a key the window does not
know is a refusal rather than a silent drop.

The load-bearing invariant under all of this is **one source, many doors**, and the source is the
`reuben-api` **window**. It declares the argument and result types every door serializes, the one
sentence each verb is advertised by, the roster that names them, the `expect` guard, and both ends of
the structure channel the engine verbs cross — so the native CLI, the MCP sidecar, the web in-page
tool layer, and the web proxy all generate their schemas from that one source and no verb means
different things behind different doors. MCP is one door, not the contract — web parity ports the
contracts, not the protocol. What a door still decides is what the window cannot know for it: its own
transport, the shape of its surface when it merges or omits verbs, and the resource store it hands in.
The window's own surface carries no completeness test — the consumer that needed a verb is the report
that one is missing. Grounding is **single-sourced** the same way: normative prose lives once (the
authoring guide, the intent vocabulary, the library index), and code, skills, and server
`instructions` **gist-and-point** at it rather than restating it.

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

<a id="window-declares-its-own-types"></a>
### The window declares the types every consumer sees and their serialization, and the engine's equivalents stay internal — the API is not a projection of the engine's surface and owes it no mirror.

[why](rationale/agent-mcp/window-declares-its-own-types.md)

<a id="portable-tool-contracts"></a>
### The tool contract types, the argument and result shapes they carry, and the introspection behind them live once in reuben-api, so every door — native CLI, MCP sidecar, web in-page layer, web proxy — generates its schemas from that one source and no verb means different things behind different doors.

[why](rationale/agent-mcp/portable-tool-contracts.md)

<a id="window-owns-advertised-prose"></a>
### Every sentence a model reads — a tool's description, a field's, a `$defs` entry's — lives once in the window beside the type it describes, and a door that advertises the roster verb-for-verb carries that sentence rather than writing one.

[why](rationale/agent-mcp/window-owns-advertised-prose.md)

<a id="consumers-are-the-completeness-test"></a>
### The window's surface carries no completeness test of its own: the consumer that needed a verb is the report that it is missing, and the authoring eval's freehand-JSON count is the measurement that stands in for one.

[why](rationale/agent-mcp/consumers-are-the-completeness-test.md)

<a id="door-owned-shape-and-store"></a>
### A door decides only what the window cannot know for it: the shape of its own surface, so a door that merges or omits verbs writes its own help, and the resource store it hands in, so the door that will play a document decodes what a door that only authors it stats.

[why](rationale/agent-mcp/door-owned-shape-and-store.md)

<a id="structure-channel-is-loopback-only"></a>
### The structure channel binds loopback only, because structure edits are strictly more powerful than OSC control — and its one default address is shared with the wire types both ends serialize so the server and client can never drift apart.

[why](rationale/agent-mcp/structure-channel-is-loopback-only.md)

<a id="structure-channel-envelope"></a>
### Both ends of the structure channel are the window's — the verbs, their payloads, the framing, the default address and the batch bound — so what a swap means is written once rather than on whichever side happened to serve it.

[why](rationale/agent-mcp/structure-channel-envelope.md)

<a id="optimistic-concurrency-guard"></a>
### The optimistic-concurrency expect guard is the window's, applied inside the Coordinator lock so the compare and the swap are one critical section — every door that can ask a Coordinator what it has installed shares that one implementation, and only a lane that cannot reach an installed hash writes its own.

[why](rationale/agent-mcp/optimistic-concurrency-guard.md)

<a id="document-verbs"></a>
### The agent authors through a closed vocabulary of path-addressed, stateless, engine-free document verbs, each applying one surgical edit to the named source, re-validating the whole document through the loader, and writing only if it is valid.

[why](rationale/agent-mcp/document-verbs.md)

<a id="closed-argument-surface"></a>
### Every verb's argument surface is closed — an argument the window does not declare is a refusal, never a silently dropped key — because a wrong document reported as written has nothing downstream to catch it.

[why](rationale/agent-mcp/closed-argument-surface.md)

<a id="value-verbs-one-address-space"></a>
### Every input is addressable through one address space — an interface pipe is the node it mints, port `in` — and a verb whose operation is meaningless on an address refuses by naming what that address is: a boundary input cannot be wired because it is fed from outside the graph, and a wired input is never silently severed.

[why](rationale/agent-mcp/value-verbs-one-address-space.md)

<a id="echo-change-not-state"></a>
### A verb echoes the state when it wrote fields the caller did not specify, and echoes the change — from and to — when the caller specified exactly what changed.

[why](rationale/agent-mcp/echo-change-not-state.md)

<a id="document-projection"></a>
### The agent never loads a reuben-owned document into its context: its whole view is a set of partial structural projections — index, node zoom with reverse edges, pipes, resources — single-sourced in reuben-core and lossless only in aggregate.

[why](rationale/agent-mcp/document-projection.md)

<a id="one-serialization-per-view"></a>
### A door serves the rendered projection rather than a second encoding of it, and a view stays structured only where a program parses it.

[why](rationale/agent-mcp/one-serialization-per-view.md)

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
### Musical intent language grounds in one curated, registry-keyed word-to-move table — instrument-blind, delivered in-prompt for reading and applied by the engine as one atomic batch of value edits for writing — kept referentially fresh by CI and musically fresh by evals.

[why](rationale/agent-mcp/intent-vocabulary.md)

<a id="cross-lane-grounding"></a>
### Grounding splits by direction, not persona: input handling (reading intent as moves) is shared base sauce delivered to every lane, while output filtering (the sound-not-machine persona) is host-owned flavor.

[why](rationale/agent-mcp/cross-lane-grounding.md)

<a id="grounding-not-schema"></a>
### There is no instrument JSON Schema for agent grounding; an agent grounds on prose rules, operator ports, and the validator loop, and registry truth is guarded by same-commit native-versus-wasm describe parity.

[why](rationale/agent-mcp/grounding-not-schema.md)

<a id="grounding-budget-is-relative"></a>
### A grounding projection's size is gated relative to the full view it compresses, never as a flat per-item cap — so the lever on a budget the registry has outgrown is the projection's own compression, not the number of operators — **not built: the only gate is an `#[ignore]`d test, so nothing currently watches the listing's size.**

[why](rationale/agent-mcp/grounding-budget-is-relative.md)

<a id="conversational-loop-verification"></a>
### The conversational authoring loop is verified by a fixed menu — live-channel integration tests, Coordinator-direct behavioral swap checks, allocation-counting for RT-safety, and scripted human rituals where automation cannot reach.

[why](rationale/agent-mcp/conversational-loop-verification.md)

## Terms

- **Sidecar** — the disposable per-conversation MCP stdio process the client spawns: pure tools in-process, engine tools forwarded to the user-owned engine.
- **Window** — the `reuben-api` crate: the one thing between the engine and every consumer, declaring the types a door serializes, the roster and its advertised sentences, and both ends of the structure channel.
- **Door** — one surface over the window's contract types (native CLI, MCP sidecar, web in-page layer, web proxy); no verb means different things behind different doors.
- **Gist-and-point** — the anti-drift posture for prose that must live in code: carry the one-breath gist and point at the single canonical doc, never restate it.
- **Intent vocabulary** — the one curated, registry-keyed word→move table that grounds musical/mood words (warmer, busier, sadder) as operator-type parameter moves.
- **Intent word** — one word of that table, applied as the whole ordered batch of value edits its row names: the engine does the word→ports→arithmetic join, not the model.
- **Input handling** — interpreting musical, mood, or abstract language as patching moves; the shared base grounding identical in every lane.
- **Output filter** — the host-owned persona: what the person is shown (sound-not-machine subject, hidden diagnostics, register), maximal on web and absent at skills/MCP.
- **Delivery lane** — a grounding consumer (repo skills, MCP clients, web chat), each reducing to transport bindings plus host furniture plus the shared base sauce, fed by push or pull.
- **Document verb** — one member of the closed, format-derived vocabulary an agent authors with: a stateless `(source, …)` mutator that applies one surgical edit, re-validates the whole document, and writes iff valid.
- **Structural projection** — the agent's whole view of a document: partial per view (index, node zoom with reverse edges, pipes, resources), lossless only in aggregate, and single-sourced in reuben-core.
