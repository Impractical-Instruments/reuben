# MCP server in Rust: landscape research

**Date:** 2026-07-10 (all sources accessed this day) ·
**Ticket:** [#271](https://github.com/Impractical-Instruments/reuben/issues/271) (wayfinder MCP/A)

**Research question:** What is the current state of building an MCP (Model Context Protocol)
server in Rust, and what fits reuben's charter posture — `reuben-core` stays untouched and
OS-free; the boundary adapter (`crates/reuben-native`) may take rmcp + tokio *only if it earns
its weight*?

Method: primary sources only — the MCP spec source ([modelcontextprotocol/modelcontextprotocol](https://github.com/modelcontextprotocol/modelcontextprotocol),
fetched from the repo because modelcontextprotocol.io 403s through our proxy), the
[official Rust SDK repo](https://github.com/modelcontextprotocol/rust-sdk), crates.io API data,
the actual repos of comparable audio-engine MCP servers, and an empirical dependency-weight
measurement done in a scratch crate today. Claims that could not be verified against a primary
source are flagged as such.

---

## 0. The spec is moving *right now* — revision list first

The protocol has shipped four revisions and has a fifth landing in eighteen days. This context
gates everything below.

| Revision | Status (2026-07-10) | One-line gist |
|---|---|---|
| **2024-11-05** | superseded | Initial release: JSON-RPC 2.0, stdio + HTTP+SSE transports, tools/resources/prompts. |
| **[2025-03-26](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-03-26/changelog.mdx)** | superseded | OAuth 2.1 auth framework; **Streamable HTTP replaces HTTP+SSE**; JSON-RPC **batching added**; tool annotations; audio content type. |
| **[2025-06-18](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-06-18/changelog.mdx)** | superseded | JSON-RPC **batching removed** (PR #416); **structured tool output** (`structuredContent`/`outputSchema`, PR #371); elicitation; resource links in tool results; `MCP-Protocol-Version` header for HTTP; lifecycle compliance SHOULD→MUST. |
| **[2025-11-25](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/changelog.mdx)** | **current** | Experimental **tasks**; sampling gains tool calling; URL-mode elicitation; icons metadata; OIDC discovery; stdio servers may use stderr for all logging; JSON Schema 2020-12 default dialect; formal governance + SDK tiering. |
| **2026-07-28** | RC, locked 2026-05-21, publishes 2026-07-28 | **Stateless core**: the `initialize` handshake is *removed* (version/identity/capabilities travel in `_meta` on every request); new mandatory `server/discover`; `Mcp-Session-Id` removed; extensions framework (reverse-DNS IDs); Tasks graduates; MCP Apps. Deliberately breaking — "the kind of foundational change that needed a clean break" — but introduces a formal deprecation policy of "at least twelve months between deprecation and the earliest possible removal" going forward. ([RC announcement](https://blog.modelcontextprotocol.io/posts/2026-07-28-release-candidate/)) |

Churn assessment for a **stdio server specifically**: the *wire framing* has never changed
(newline-delimited JSON since 2024-11-05, see §2), and stdio dodges every auth/session/transport
change — those are the HTTP server's problem. But the *message-level* surface is not calm:
batching was added (2025-03-26) then removed (2025-06-18), and the 2026-07-28 release removes
the `initialize` handshake itself — the one ceremony every stdio server implements. A stdio
server pinned to 2025-11-25 keeps working only as long as clients keep negotiating old versions
(version negotiation happens in `initialize`; the RC's 12-month deprecation policy suggests a
long tail, but that policy is new and untested).

---

## 1. rmcp — the official Rust SDK

### Maturity

- **Current:** rmcp **2.2.0**, released 2026-07-08. Crate created 2025-03-16; 50 releases;
  **15.3M total downloads** (8.0M recent). Apache-2.0. Repo: [modelcontextprotocol/rust-sdk](https://github.com/modelcontextprotocol/rust-sdk)
  (3.6k stars, 557 forks). ([crates.io API](https://crates.io/api/v1/crates/rmcp))
- **Release cadence & stability:** 0.x until 0.8.5 (2025-11-05) → **1.0.0 on 2026-03-03** →
  **2.0.0 on 2026-06-29** → 2.1.0 (07-02) → 2.2.0 (07-08). ([versions API](https://crates.io/api/v1/crates/rmcp/versions))
  So: out of 0.x only four months ago, and a breaking major three months after 1.0. The 2.0.0
  breaking change was "align model types with MCP 2025-11-25 spec" (with a
  [migration guide](https://github.com/modelcontextprotocol/rust-sdk/discussions/926)); the
  macros crate flagged further breaking model-type alignment in 2.1/2.2.
  ([releases](https://github.com/modelcontextprotocol/rust-sdk/releases)) Semver majors track
  *spec* churn, not API instability for its own sake — but expect another major for 2026-07-28.
- **Governance position:** Rust is a **Tier 2** SDK ([modelcontextprotocol.io/docs/sdk](https://modelcontextprotocol.io/docs/sdk);
  Tier 1 = TypeScript, Python, C#, Go). Tier 2 commits to ≥80% conformance-test pass, at least
  one stable release, and "new protocol features implemented within six months"
  ([SEP-1730](https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1730)).
  Consistent with observation: 2.0.0's alignment with the 2025-11-25 spec landed ~7 months after
  that revision. The RC blog says "Tier 1 SDKs are expected to ship support within this window"
  (the 10-week RC window) — Rust, as Tier 2, has the longer six-month leash.
- The [README](https://github.com/modelcontextprotocol/rust-sdk) states it implements spec
  revision **2025-11-25**.

### API shape

From the repo's [stdio example](https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/servers/src/counter_stdio.rs)
and its [common counter module](https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/servers/src/common/counter.rs):

- A server is a plain struct holding a generated `ToolRouter<Self>`; a `#[tool_router]` impl
  block declares tools as methods with `#[tool(description = "…")]`; typed params come in as
  `Parameters<T>` where `T: serde::Deserialize + schemars::JsonSchema` (the input schema is
  derived, not hand-written); tools return `Result<CallToolResult, McpError>`.
- `impl ServerHandler` (decorated `#[tool_handler]`) supplies `get_info()` with a
  `ServerCapabilities::builder().enable_tools()…` capability declaration. Prompts get the same
  treatment via `#[prompt_router]`/`#[prompt]`; there is a `#[task_handler]` for the new tasks
  extension.
- `main` is `#[tokio::main] async fn main()`, and serving is one line:
  `Counter::new().serve(stdio()).await?` then `service.waiting().await?`. Logging goes to
  stderr via `tracing` (correct for stdio — see §2 framing rules).

### Dependency weight (measured today, not estimated)

From [crates/rmcp/Cargo.toml](https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/Cargo.toml):

- **Required deps:** async-trait, serde (+derive), serde_json, thiserror, **tokio 1 with
  features `["sync", "macros", "rt", "time"]`**, futures, tracing, tokio-util, pin-project-lite,
  chrono. Default features = `["base64", "macros", "server"]`; `server` pulls in schemars +
  pastey.
- **Transport gating is clean:** `transport-io` (stdio) needs only `transport-async-rw` +
  `tokio/io-std` (+`io-util`, `tokio-util/codec`). Everything HTTP — hyper, reqwest, sse-stream,
  uuid, oauth2, jsonwebtoken — sits behind `transport-streamable-http-*`, `server-side-http`,
  and `auth` features and is **not** compiled for a stdio-only server.
- **The tokio commitment is smaller than "tokio" sounds.** The crate's declared tokio features
  are `sync/macros/rt/time` plus `io-std`/`io-util` for stdio — notably **not**
  `rt-multi-thread` and **not** `net`. At the dependency level a **`current_thread` runtime
  suffices**; the examples use plain `#[tokio::main]` (which defaults to the multithreaded
  runtime), but nothing in the Cargo manifest demands it. (I found no primary-source statement
  *documenting* current-thread support; the manifest is the evidence.)
- **Measured lock impact** (scratch crate, `rmcp = { version = "2", features = ["transport-io"] }`,
  defaults on, `cargo generate-lockfile`, 2026-07-10): **74 locked packages** total, and the
  resolved tree contains **no mio, no socket2, no libc network stack** — stdio-only rmcp
  carries no sockets. Diffed against reuben's current 180-package `Cargo.lock`:
  **34 packages are new** (tokio, tokio-util, tokio-macros, futures ×7, tracing ×3, schemars ×2,
  chrono, async-trait, base64, darling ×3, pastey, ref-cast ×2, dyn-clone, rmcp ×2, plus
  platform-conditional iana-time-zone/windows-*/android crates). A large share are
  compile-time-only proc-macro crates. reuben's workspace lock currently contains **zero tokio**
  (verified by grep).

### Alternatives (one line each)

- [rust-mcp-sdk](https://crates.io/crates/rust-mcp-sdk) — 0.10.0 (2026-06-24), 189k downloads;
  claims full 2025-11-25 support and passing the official conformance suite; ~80× fewer
  downloads than rmcp.
- [mcpkit](https://github.com/praxiomlabs/mcpkit) — community SDK reducing boilerplate to a
  single `#[mcp_server]` macro; targets 2025-11-25.
- [mcp-sdk-rs](https://www.shuttle.dev/blog/2025/09/15/mcp-servers-rust-comparison) — low-level
  community crate exposing protocol types/traits without a framework.
- **Why rmcp is the default:** it is the SDK the protocol org itself hosts and tiers, with an
  explicit (Tier 2) commitment to track spec changes, and adoption two orders of magnitude
  beyond any alternative. No community crate offers a churn-absorption story rmcp doesn't.

---

## 2. The hand-rolled alternative

### What a minimal conformant stdio server must implement

MCP is JSON-RPC 2.0. Per the current (2025-11-25) spec, the framing for stdio is trivially
simple — **there is no `Content-Length` header** (this is not LSP): messages are individual
JSON-RPC objects "delimited by newlines, and MUST NOT contain embedded newlines"; the server
"MUST NOT write anything to its stdout that is not a valid MCP message"; stderr is free for
logging ([transports](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/transports.mdx)).
So the transport is: read stdin line-by-line, `serde_json::from_str`, dispatch, write one line.

Mandatory message surface for a tools-only server
([lifecycle](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/lifecycle.mdx),
[tools](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/tools.mdx)):

1. **`initialize`** — respond with negotiated `protocolVersion`, declared capabilities,
   `serverInfo`. Lifecycle compliance has been MUST since 2025-06-18.
2. **`notifications/initialized`** — accept (no response).
3. **`ping`** — the mechanism is "optional," but "the receiver MUST respond promptly with an
   empty response" — so handling it is effectively mandatory
   ([ping](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/ping.mdx)).
4. **`tools/list`** — static array; pagination is cursor-based and a server that returns
   everything in one page simply omits `nextCursor`.
5. **`tools/call`** — dispatch; distinguish protocol errors (JSON-RPC error) from tool
   execution errors (`isError: true` result, which "clients SHOULD provide … to language models
   to enable self-correction").

Everything else is **capability-gated and legitimately omittable**: resources and prompts exist
only if declared (and within resources, `subscribe` and `listChanged` are "entirely optional —
servers can support neither, either, or both"
([resources](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/resources.mdx)));
logging, completions, and tasks are separate optional capabilities; progress notifications are
optional; and `notifications/cancelled` receivers "SHOULD" stop processing but "MAY ignore
cancellation notifications"
([cancellation](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/cancellation.mdx)).
**JSON-RPC batching need not be considered at all**: added in 2025-03-26, removed in 2025-06-18
([changelog](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-06-18/changelog.mdx), PR #416).

### Scope, honestly, against the repo's precedent

reuben already hand-rolls a protocol at this boundary:
[`crates/reuben-native/src/osc.rs`](../../crates/reuben-native/src/osc.rs) is **196 lines
including its tests** (~100 lines of code) over the `rosc` codec crate. A hand-rolled MCP stdio
server is bigger but the same *kind* of thing: a `stdin().lock().lines()` loop,
`serde_json::Value` (or small serde structs) dispatch over ~5 methods, serde-serialized results
— for describe/validate-shaped tools this is single-client, strictly sequential, no concurrency
required. Realistic estimate: **300–600 lines, zero new dependencies** (serde_json is already
in the workspace), i.e. on the order of the existing 513-line
[`bin/reuben.rs`](../../crates/reuben-native/src/bin/reuben.rs). ADR-0020's design already
provides the tool bodies: `describe`/`validate` are pure library functions returning
serde-serializable reports.

The asymmetry with OSC: OSC 1.0 has been frozen since ~2002; MCP has shipped four revisions in
twenty months and is deleting `initialize` — the heart of the hand-rolled surface — on
2026-07-28 (§0). Hand-rolling transfers churn-tracking from rmcp's maintainers to this repo.
The counterweight: a pinned hand-rolled server keeps working under version negotiation for as
long as clients honor old protocol versions, and the surface to re-track is five methods.

---

## 3. Transports and lifecycle — the live-audio-engine wrinkle

### What the spec says

- **stdio:** the client launches the server as a subprocess. Shutdown: the client closes the
  server's stdin, waits, then SIGTERM, then SIGKILL
  ([lifecycle](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/lifecycle.mdx)).
  **Consequence: whatever lives in the server process dies with the client conversation.** One
  client per process by construction. Claude Code additionally does *not* auto-restart stdio
  servers ("Stdio servers are local processes and are not reconnected automatically" —
  [Claude Code MCP docs](https://code.claude.com/docs/en/mcp)).
- **Streamable HTTP:** standalone long-running process; clients connect over POST/GET; the
  server may issue an `MCP-Session-Id` header at initialization which clients "MUST include …
  on all of their subsequent HTTP requests"; resumability via SSE event IDs + `Last-Event-ID`;
  multiple independent clients are natural
  ([transports](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/transports.mdx)).
  Claude Code auto-reconnects HTTP servers with exponential backoff (up to five attempts).
  **Caveat:** the 2026-07-28 RC removes `Mcp-Session-Id` and protocol-level sessions entirely
  ([RC post](https://blog.modelcontextprotocol.io/posts/2026-07-28-release-candidate/)) — do
  not build against the session header as a long-term surface.
- **HTTP+SSE (2024-11-05 transport) is deprecated:** Streamable HTTP "replaces the HTTP+SSE
  transport from protocol version 2024-11-05" (2025-03-26 changelog + 2025-11-25 transports).

### Who starts the sound / what happens on client exit / can two clients share one engine

| | stdio, engine in-process | stdio shim → socket → engine | Streamable HTTP, engine in-process |
|---|---|---|---|
| Who starts the sound | MCP client spawn starts the engine | User (or anything) starts the engine; shim just connects | User/daemon starts the server once |
| Client exits | **Sound stops** (stdin closes → process exits) | Shim dies; **engine keeps playing** | Connection closes; engine keeps playing |
| Two clients, one engine | No — one subprocess per client, each with its own engine | Yes — shims are cheap, socket is shared | Yes — that's the transport's design |

### The comparable servers (all repos fetched 2026-07-10)

- **[ahujasid/ableton-mcp](https://github.com/ahujasid/ableton-mcp)** (Ableton Live): Python
  FastMCP over **stdio**, bridging to "a MIDI Remote Script for Ableton Live that creates a
  socket server" — TCP `localhost:9877`
  ([server.py](https://github.com/ahujasid/ableton-mcp/blob/main/MCP_Server/server.py):
  `ABLETON_PORT … "9877"`), with 3-attempt reconnect logic. The user starts Ableton; the MCP
  process never does.
- **[abhishekjairath/sonic-pi-mcp](https://github.com/abhishekjairath/sonic-pi-mcp)** (Sonic
  Pi): TypeScript/Node over **stdio**, sends generated code to Sonic Pi **over OSC** (port
  4560, `/run-code`, `/stop-all-jobs`). Sonic Pi "must already be running" after a one-time
  manual buffer setup.
- **[Tok/SuperColliderMCP](https://github.com/Tok/SuperColliderMCP)** (SuperCollider): Python
  (FastMCP + python-osc) over **stdio**, **OSC to scsynth on port 57110**; "Ensure server is
  running on port 57110" — engine pre-started by the user.
- **[Synohara/supercollider-mcp](https://github.com/Synohara/supercollider-mcp)** — the
  counterexample: TypeScript over stdio that **boots the engine itself** via supercolliderjs
  (`sc.server.boot({...})` in
  [src/index.ts](https://github.com/Synohara/supercollider-mcp/blob/main/src/index.ts)), so the
  audio engine's lifetime is the conversation's lifetime.
- **[tiianhk/MaxMSP-MCP-Server](https://github.com/tiianhk/MaxMSP-MCP-Server)** (Max/MSP):
  Python MCP server (stdio) ↔ **Socket.IO** ↔ JavaScript (V8) running inside Max 9; the user
  opens the Max patch and clicks "script start."
- **[signalcompose/maxmcp](https://github.com/signalcompose/maxmcp)** (Max/MSP, native): a C++
  external *inside* Max acts as a **WebSocket server** (default port 7400); a Node.js bridge
  (`websocket-mcp-bridge.js`) speaks **stdio** to Claude and WebSocket to the external.

**The pattern is near-universal: five of six use a thin, disposable MCP stdio shim in a
scripting language, bridging over a local socket (TCP / OSC / Socket.IO / WebSocket) to a
long-lived engine process the user starts.** The engine survives client disconnects; the shim
is spawned per-conversation and nobody cares when it dies. The one exception (Synohara) accepts
engine-dies-with-conversation. None of the surveyed servers use Streamable HTTP.

Notably, reuben already has the pattern's right-hand side built: `reuben play` is a long-lived
engine process listening for OSC on UDP `0.0.0.0:9000`
([bin/reuben.rs](../../crates/reuben-native/src/bin/reuben.rs)), and ADR-0039's
`Engine::from_document`/`queue_osc` embed surface additionally makes an in-process engine
possible for whoever wants the Synohara shape.

---

## 4. Tool / resource / prompt conventions

### Tools returning large JSON (the `describe_operators` case)

- **`structuredContent` + `outputSchema`** were added in **2025-06-18** ("Add support for
  structured tool output", PR #371). If a tool declares `outputSchema`, "Servers MUST provide
  structured results that conform to this schema," and a tool returning structured content
  "SHOULD also return the serialized JSON in a TextContent block" for backwards compatibility
  ([tools](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/tools.mdx)).
  For reuben this is a direct fit: `describe`/`validate` already return serde-serializable
  reports (ADR-0020), i.e. the `structuredContent` payload exists today.
- **The spec has no size guidance for single tool results** (only `tools/list` pagination).
  Size limits are a *client* norm: **Claude Code warns at 10,000 tokens of tool output and
  hard-caps at 25,000 tokens by default** (`MAX_MCP_OUTPUT_TOKENS`), with a per-tool
  `anthropic/maxResultSizeChars` annotation escape hatch, and advises server authors "to
  paginate their responses" ([Claude Code MCP docs](https://code.claude.com/docs/en/mcp)).
  Practical rule: a full-registry schema dump must fit ~25k tokens or be split — the CLI's
  existing shape (`describe` one operator at a time, list-all as a cheap index) is already the
  right granularity.

### Resources for authoring guidance

- Resources are **application-driven** by design: "host applications determin[e] how to
  incorporate context based on their needs," and the protocol "does not mandate any specific
  user interaction model"
  ([resources](https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/resources.mdx)).
  Templates are RFC 6570 URI templates via `resources/templates/list`.
- How clients actually surface them (Claude Code, per its
  [docs](https://code.claude.com/docs/en/mcp)): resources are **not auto-read**. Users
  reference them with `@server:scheme://path` mentions ("Resources are automatically fetched
  and included as attachments **when referenced**"), and "Claude Code automatically provides
  tools to list and read MCP resources" — so the *model* can also pull them on demand. MCP
  prompts surface as `/mcp__server__prompt` slash commands.
- Rule of thumb this implies: **resources for stable, human-or-model-browsable documents**
  (authoring guide, per-operator docs — things an agent should be able
  to `@`-mention or list); **tools for anything computed or parameterized** (validate,
  describe-this-patch). A tool returning text is model-driven and costs a tool call; a resource
  is addressable context. Given tool-search defers tool schemas, server `instructions` +
  resources carry the discoverability load.

### Binary content (the sample-upload case)

- **Server → client** is well-specified: resources carry binary as a base64 `blob` field with
  a `mimeType`; tool results can embed base64 `image`/`audio` content blocks (audio content
  added in 2025-03-26).
- **Client → server has no binary primitive.** Tool arguments are JSON validated against
  `inputSchema` — so binary uploads are either base64-in-a-string or a reference the server can
  resolve. I found **no primary-source convention document** for uploads; the observable norm
  in the surveyed audio servers is **file-path passing** — every one of them runs on the same
  machine as its engine and passes paths/names over the bridge rather than payloads. For a
  local stdio server this is also what the client-side `roots` capability anticipates (clients
  advertise filesystem roots to servers). For reuben, whose resolver already loads samples by
  path (`FsResolver`, ADR-0016), path-passing is both the ecosystem norm and the zero-new-code
  option; base64 tool args remain viable for small payloads at ~33% size overhead.

---

## 5. What this means for reuben

*Evidence, not a verdict — the decisions belong to MCP/B (server location & lifecycle) and
MCP/E.*

**Charter constraints, restated from the ADRs (one line each):**
- [ADR-0007](../adr/0007-osc-only-core.md): the core speaks only OSC-shaped Messages; MCP must
  be an isolated, removable boundary adapter that converts to/from OSC at the edge.
- [ADR-0012](../adr/0012-boundary-and-threading.md): the MCP adapter lives in the I/O & control
  region, crossing to Render only via the existing lock-free queues — nothing about MCP may
  touch core threading.
- [ADR-0020](../adr/0020-introspection-and-patcher-skill.md): `describe`/`validate` are pure
  library functions with serde report types; an MCP tool surface is a second thin shell over
  the same functions, not new capability.
- [ADR-0039](../adr/0039-engine-in-core-embed-surface.md): `Engine::from_document` +
  `queue_osc(address, &[Arg])` is the portable embed surface — an MCP process could host an
  engine in-process without touching core.

**rmcp vs hand-rolled — where the evidence leans.** The measured cost of rmcp is lower than
its reputation: 34 new lock packages (many compile-time-only, none of them a network stack),
tokio confined to `sync/rt/time/io-std` with no `rt-multi-thread` requirement, all HTTP machinery
feature-gated off, and everything contained in `reuben-native` exactly as the charter allows.
The hand-rolled server is genuinely small *today* (~5 methods, newline-delimited JSON, zero new
deps, same order of magnitude as the existing hand-rolled OSC codec and the `reuben` binary) —
but the OSC precedent doesn't transfer cleanly: OSC's spec has been frozen for two decades,
while MCP has shipped four revisions in twenty months, deleted a feature it added three months
earlier (batching), and is removing the `initialize` handshake itself on **2026-07-28** —
eighteen days from now. Whoever owns the wire surface owns that churn. rmcp's whole value is
absorbing it, under a published Tier-2 commitment ("new protocol features within six months");
its cost is riding its majors (three majors in four months, each tracking a spec revision). On
the charter's own test — "rmcp + tokio only if it earns its weight" — the evidence leans toward
*rmcp earning it* for any server that will live past this summer's protocol break, and toward
hand-rolling only if MCP/B picks a shape so minimal (tools-only, pinned revision, disposable)
that re-tracking five methods is cheaper than riding majors. Deferring the build until rmcp
ships 2026-07-28 support would avoid implementing the old lifecycle twice.

**Lifecycle options for MCP/B, framed by §3.** (a) *stdio, engine in-process* (the Synohara
shape): simplest, and ADR-0039 makes it cheap — but the spec's shutdown semantics mean the
sound dies when the conversation ends, and two clients can never share an engine.
(b) *stdio shim → OSC/UDP → `reuben play`*: the pattern five of six comparable audio servers
converged on, and reuben uniquely already has the engine side built and listening on UDP 9000 —
the shim is disposable, the engine survives disconnects, `REUBEN_LOG_OSC` debugging still works,
and the shim needs no audio code at all. (c) *Streamable HTTP, engine in-process*: solves
multi-client and persistence in one process, but buys the auth/session surface — the churniest
part of the spec (sessions are being removed from the protocol this month) — and no surveyed
audio server chose it. The evidence says the ecosystem's answer to "live engine + disposable
conversations" is (b); whether reuben wants (a)'s zero-IPC simplicity for validate/describe-only
tooling versus (b)'s survivability for actual sound is exactly the MCP/B question.

---

## Sources

All accessed 2026-07-10.

**Spec (source of modelcontextprotocol.io, fetched from GitHub):**
- 2025-03-26 changelog — https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-03-26/changelog.mdx
- 2025-06-18 changelog — https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-06-18/changelog.mdx
- 2025-11-25 changelog — https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/changelog.mdx
- 2025-11-25 transports — https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/transports.mdx
- 2025-11-25 lifecycle — https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/lifecycle.mdx
- 2025-11-25 tools — https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/tools.mdx
- 2025-11-25 resources — https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/server/resources.mdx
- 2025-11-25 ping — https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/ping.mdx
- 2025-11-25 cancellation — https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/docs/specification/2025-11-25/basic/utilities/cancellation.mdx
- 2026-07-28 release candidate announcement — https://blog.modelcontextprotocol.io/posts/2026-07-28-release-candidate/
- SDK tiers (Rust = Tier 2) — https://modelcontextprotocol.io/docs/sdk (via repo docs/docs/sdk.mdx); SEP-1730 tier definitions — https://github.com/modelcontextprotocol/modelcontextprotocol/issues/1730

**rmcp / Rust SDKs:**
- Repo + README — https://github.com/modelcontextprotocol/rust-sdk
- Manifest — https://github.com/modelcontextprotocol/rust-sdk/blob/main/crates/rmcp/Cargo.toml
- Releases — https://github.com/modelcontextprotocol/rust-sdk/releases ; 2.0 migration guide — https://github.com/modelcontextprotocol/rust-sdk/discussions/926
- Examples — https://github.com/modelcontextprotocol/rust-sdk/blob/main/examples/servers/src/counter_stdio.rs , …/common/counter.rs
- crates.io API — https://crates.io/api/v1/crates/rmcp , https://crates.io/api/v1/crates/rmcp/versions , https://crates.io/api/v1/crates/rust-mcp-sdk
- mcpkit — https://github.com/praxiomlabs/mcpkit ; SDK comparison — https://www.shuttle.dev/blog/2025/09/15/mcp-servers-rust-comparison
- Dependency measurement: scratch crate with `rmcp = { version = "2", features = ["transport-io"] }`, `cargo generate-lockfile`, 2026-07-10 (74 packages locked; 34 not present in reuben's Cargo.lock).

**Comparable servers:**
- Ableton — https://github.com/ahujasid/ableton-mcp (+ MCP_Server/server.py for port 9877)
- Sonic Pi — https://github.com/abhishekjairath/sonic-pi-mcp
- SuperCollider (OSC bridge) — https://github.com/Tok/SuperColliderMCP
- SuperCollider (engine-in-process) — https://github.com/Synohara/supercollider-mcp (+ src/index.ts for `sc.server.boot`)
- Max/MSP (Socket.IO bridge) — https://github.com/tiianhk/MaxMSP-MCP-Server
- Max/MSP (native external + WS bridge) — https://github.com/signalcompose/maxmcp

**Client behavior:**
- Claude Code MCP docs (resources via @-mentions, prompts as slash commands, 10k-token warning / 25k-token default cap, `MAX_MCP_OUTPUT_TOKENS`, stdio non-reconnection) — https://code.claude.com/docs/en/mcp

**reuben (local, read 2026-07-10):** docs/adr/0007, 0012, 0020, 0039; crates/reuben-native/src/osc.rs (196 lines incl. tests); crates/reuben-native/src/bin/reuben.rs (513 lines); crates/reuben-native/Cargo.toml; workspace Cargo.toml + Cargo.lock (180 packages, no tokio).
