# ADR-0044: MCP server is a stdio sidecar; the engine stays user-owned

## Status

Accepted (2026-07-11). The server-location & lifecycle decision of the reuben MCP server
effort — wayfinder ticket [MCP/B (#273)](https://github.com/Impractical-Instruments/reuben/issues/273)
on map [#270](https://github.com/Impractical-Instruments/reuben/issues/270), deciding open
question 1 of [#220](https://github.com/Impractical-Instruments/reuben/issues/220) — grounded
on the [MCP/A landscape research](https://github.com/Impractical-Instruments/reuben/blob/d0c5ffcdf9956e6466c65aeaf4bb5c8e63454bcb/docs/research/mcp-rust-server-landscape.md)
([#271](https://github.com/Impractical-Instruments/reuben/issues/271)). **Rides on**
[ADR-0007](0007-osc-only-core.md) (OSC-only core), [ADR-0012](0012-boundary-and-threading.md)
(single-writer Coordinator, removable I/O region), [ADR-0020](0020-introspection-and-patcher-skill.md)
(describe/validate as pure library functions), and [ADR-0039](0039-engine-in-core-embed-surface.md)
(embed surface); **amends none**. Feeds the Coordinator/Swap design (MCP/D), the tool surface
(MCP/E), sample upload (MCP/F), and web parity (MCP/I).

## Context

Conversational instrument authoring ([#220](https://github.com/Impractical-Instruments/reuben/issues/220))
needs an MCP server, and the server needs a home relative to the sound. The fork: **in-process**
with the engine (an MCP process that hosts `Engine` via ADR-0039's embed surface) vs a
**sidecar** that talks to a separately running engine. Facts that bore on it:

- MCP's stdio transport ties the server process to the conversation: the client spawns it,
  and on conversation end closes stdin → SIGTERM → SIGKILL. Whatever lives in that process
  dies with the chat.
- `describe`/`validate`/`describe_patch` are **pure functions** over `Registry` + JSON
  (ADR-0020) — they need no engine at all. Only `send` (and, in M2, structure edits and
  diagnostics) touches a live process.
- The existing OSC/UDP boundary (`reuben play`, port 9000) carries **control messages only** —
  it can serve `send` today but cannot reach a future Coordinator (ADR-0012's single writer of
  structure) or the diagnostics counters.
- The surveyed audio-engine MCP servers (Ableton, Sonic Pi, SuperCollider ×2, Max/MSP ×2)
  near-universally converge on *disposable stdio shim → local socket → user-started long-lived
  engine*; none use Streamable HTTP, whose session surface the 2026-07-28 spec release removes
  anyway. reuben already has the pattern's engine half built.
- The MCP spec is churning (the `initialize` handshake itself is deleted on 2026-07-28); rmcp
  stdio-only was measured at 34 new lock packages with no network stack, tokio confined to
  `sync/rt/time/io-std`, and a `current_thread` runtime sufficing.

## Decision

### 1. Per-conversation stdio sidecar

The MCP server is a **disposable stdio process the client spawns per conversation**. It hosts
the pure introspection functions **in-process** and forwards `send` over the existing OSC/UDP
boundary to a long-lived `reuben play` the user started. The sound survives conversation
death; conversations are cheap.

Structure ops and diagnostics do **not** ride OSC: the sidecar reaches the future Coordinator
over a **new local channel**, whose design belongs to the Coordinator/Swap ticket (MCP/D).
The OSC control plane stays as-is.

**Considered and rejected:** *engine-in-process* (`reuben serve`) — zero IPC and cheap via
ADR-0039, but the sound dies when the client closes stdin and two conversations can never
share an engine; *Streamable HTTP* — solves persistence and multi-client in one process, but
buys the churniest part of the spec (protocol sessions, removed 2026-07-28), and no surveyed
audio server chose it.

### 2. The user owns the engine

The shim never spawns, kills, or restarts `reuben play`. Tools that need the engine **fail
fast with an actionable error** ("start `reuben play`") when no engine is reachable. Since
UDP is silent about dead ports, this requires a cheap liveness probe — mechanism (e.g. a ping
in `play`'s OSC namespace) is implementation detail for the epic, but fail-fast-with-guidance
is the contract.

**Considered and rejected:** auto-spawning the engine (blurred ownership: who stops it, which
instrument loads, orphan management), including behind an opt-in flag (two documented
behaviors instead of one; the MVP persona is a dev with a checkout and a terminal).

### 3. Factoring: introspection descends into core; the adapter is a new crate

- The pure functions in `crates/reuben-native/src/cli.rs` move to **`reuben_core::introspect`**
  — the ADR-0039 Engine move repeated: they already import only core types, so core gains
  zero dependencies and zero MCP awareness. `reuben-native` re-exports them so the CLI is
  unchanged; the web player (MCP/I) and the MCP server consume them directly.
- The MCP adapter is a **new workspace bin crate `reuben-mcp`**, holding rmcp + tokio and
  depending on `reuben-core` (introspect, Registry) and `reuben-native` (`osc::encode`,
  `FsResolver`). This fences the measured 34 new lock packages away from every build of the
  play/CLI path — the workspace outside `reuben-mcp` stays tokio-free.

**Considered and rejected:** a `reuben mcp` subcommand or feature-gated bin in `reuben-native`
(the charter's literal wording sanctioned rmcp *in the boundary adapter*, but every play/CLI
build would pay the dependency tree, or a feature flag would split one binary into two build
configurations); a `reuben-introspect` crate (ADR-0039's reasoning holds — a boundary with
nothing to fence off; the rmcp boundary, by contrast, has 34 packages to fence).

### 4. Multi-client: tolerated, unarbitrated

Any number of shims may target one engine. In M1, concurrent `send`s are last-write-wins per
control — exactly the semantics of two physical OSC controllers today, documented rather than
arbitrated. In M2, structure edits are serialized by the Coordinator by construction
(ADR-0012); whether anything richer is needed (edit sessions, document-version optimistic
concurrency) is MCP/D's design space.

**Considered and rejected:** enforcing one client per engine — lease/liveness machinery over
a connectionless transport, blocking the two-conversations workflow, for no demonstrated need.

### 5. SDK: rmcp, stdio-only

The charter's condition ("rmcp + tokio only if it earns its weight") is met by measurement:
`transport-io` feature only, `current_thread` runtime, no network stack, all HTTP machinery
feature-gated off, protocol churn absorbed upstream under a published Tier-2 commitment.
Hand-rolling (~5 methods, zero new deps) wins only for code discarded before the 2026-07-28
protocol break; this shim is the epic's durable surface.

**Scheduling preference, not a gate:** rmcp will cut a breaking major for the 2026-07-28
spec release; if the build lands near that window, start on the new major rather than
implementing the removed lifecycle twice.

## Consequences

- The epic gains two engine-side work items: a liveness probe reachable by the shim, and
  (eventually) the Coordinator channel — both on `reuben play`'s side of the boundary. The
  `play` loop's park-forever shutdown is otherwise off this decision's critical path; the
  shim's own lifecycle is the spec's stdio shutdown, handled by rmcp.
- `reuben-mcp` is the first workspace member allowed an async runtime; the play/CLI/web
  builds keep their dependency trees unchanged.
- MCP/D inherits a fixed constraint set: the sidecar needs a local structure/diagnostics
  channel to the Coordinator; OSC stays control-only.
- MCP/E designs the tool surface against a fixed process model: pure tools always available,
  engine tools fail-fast when the engine is absent.
- Two conversations sharing one engine is a supported workflow from M1, with documented
  last-write-wins control semantics.
