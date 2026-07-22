# ADR-0065: Control rides the structure channel; OSC-the-wire is only the engine's foreign edge

## Status

Accepted (2026-07-22). Implemented. Decided through issue
[#588](https://github.com/Impractical-Instruments/reuben/issues/588), spun out of the
[#581](https://github.com/Impractical-Instruments/reuben/issues/581) grilling as its predecessor —
it unblocks #581's "MCP drops `reuben-native`" step.

**This reverses part of ADR-0044** (distilled into the agent-mcp rules as
[mcp-stdio-sidecar](../rules/rationale/agent-mcp/mcp-stdio-sidecar.md) and
[user-owned-engine](../rules/rationale/agent-mcp/user-owned-engine.md)): the sidecar↔engine seam is
no longer "OSC for control, a separate loopback structure channel for everything else."

## Context

`reuben play` presents two ingresses. **OSC-in** is UDP on `0.0.0.0:9000`, the foreign edge where
external controllers arrive: a hardware knob, a TouchOSC surface. The **structure channel** is
loopback-only TCP/NDJSON on `127.0.0.1:9124`, carrying core's own `Request`/`Response` types as
JSON, because structure edits are more powerful than control and must never be network-exposed.

ADR-0044 modeled the MCP sidecar's `send` tool as *external OSC control* — the same ingress a
physical controller uses — and routed it accordingly: encode core `Arg`s into OSC datagram bytes
with `rosc`, then dispatch over UDP to the engine's OSC-in port. Three consequences followed. `send`
had to be **probe-first** (ping the structure channel, then dispatch) because UDP is silent about a
dead port, making it the one tool that could not act-then-map. The sidecar had to own a **UDP socket
and an OSC wire format** purely to talk to its own peer. And `rosc` was pulled into `reuben-mcp` for
that one call — which, after #581 moves `FsResolver` out, would have been the *only* remaining tie
from `reuben-mcp` to `reuben-native` at all.

The round trip is ceremony. The engine's OSC-in path is `UDP bytes → rosc decode → flat Args →
boundary::osc_in_arg → Engine::queue_osc`; **everything after the first hop is core**. The sidecar
and the engine are our own peers that both already speak core `Arg`, and the sidecar already runs a
loopback structure channel carrying core types as JSON.

The web door had already worked this out. `reuben-web`'s in-page control channel is a hand-rolled
flat codec that explicitly refuses `rosc` and converges at `Engine::queue_osc`:

> **Not `rosc`**: the control channel is not OSC-the-binary-protocol. It carries the *flat primitive
> form* — an address plus a list of `F32`/`I32`/`Str` atoms — and nothing else. Pulling in an OSC
> packet library would buy type tags, bundles, timetags, and 4-byte padding we would immediately
> have to forbid, for a channel whose whole vocabulary is three primitives.

MCP was the only door still encoding OSC to reach its own engine.

## Decision

**Every door ships `{address, [Arg]}` in its own local framing and converges at
`Engine::queue_osc`. OSC-the-binary-protocol exists only at `reuben play`'s foreign edge** —
external controllers in, `osc_out` nodes out. The MCP sidecar's `send` becomes a structure-channel
verb.

### 1. The wire vocabulary is three primitives, spelled locally

`reuben_core::coordinator::wire` gains `ControlArg` — `I32 | F32 | Str`, and nothing else — plus
`ControlMessage { address, args }`.

Deliberately **not** core's `Arg`. `Arg` is the central engine enum and also carries `Note`,
`Harmony`, `Pitch`, `Enum`, and a whole `F32Buffer`: a vocabulary a control wire would
immediately have to forbid, which is the web codec's argument verbatim. Nor is `Arg` serializable
today, and making it so — hanging serde on the type the render thread passes around, for one
channel's benefit — is the wrong direction. How primitives are spelled on *this* NDJSON channel is
definitionally this channel's shape choice, which is what `wire` is for.

`ControlArg` is `#[serde(untagged)]`, so an atom rides as its bare JSON value (`[800.0, "up", 3]`).
Two reasons: the channel stays netcat-debuggable, which its framing exists to serve; and the
`I32`/`F32` split falls out of JSON's own integer-vs-float spelling rather than a rule each client
reimplements — the sidecar had hand-written exactly that policy. **Variant order is load-bearing**
(`I32` before `F32`), and is pinned by a test.

### 2. The verb is batched, and the batch is one unit end to end

`Request::Send { messages: Vec<ControlMessage> }` → `Response::Sent`.

The tool contract is already a batch — the authoring gesture is multi-control. Per-message would
mean N TCP connects and N spawned engine-side threads to replace one `UdpSocket::bind` that
dispatched N datagrams, and would split one gesture into N independently-failable steps.

Batching only earns its atomicity claim if the batch stays whole *past* the exchange, so the
render-callback ingress carries a `ControlBatch` (one gesture) rather than a message: `handle_send`
pushes once. One push is what makes all three promises true rather than aspirational — concurrent
handler threads cannot interleave into each other's gestures, the callback cannot apply half a
gesture to one block and half to the next, and there is no partial-failure window where some
messages are queued but the client is told the batch failed. The UDP producer gets the same
treatment, which also fixes a latent oddity: an OSC **bundle** means "these are simultaneous", and
its messages used to be pushed one at a time and could straddle a block.

The ack is a **unit**. A `count` could only ever equal the request's own `messages.len()` — the
batch is queued as one unit, so no partial outcome exists for a count to describe, and every
rejecting path answers `Error` instead. A field that can hold exactly one value is not information;
it invites a client to read `count < len` as "some were rejected", a signal that can never arrive.

"Queued", not "applied": an address routing to no node/port is dropped at the engine's ingress,
exactly as a stale external datagram is.

### 2b. The batch is bounded at both ends

A batch lands in a single render callback, so its size is an **RT** property, not a request-size
preference: unbounded, one authoring gesture could blow a render deadline, and "Render is RT-safe"
is non-negotiable. UDP inherited this bound for free from the kernel receive buffer; this channel
has to state it. `MAX_SEND_BATCH = 256` is shared so the door advertising a `maxItems` and the engine
enforcing it cannot drift — the engine enforces regardless, since a door is a courtesy and the
channel is the contract.

An **empty** batch is refused for a different reason: acking a no-op as success would let a client
bug that drops its messages read as a working send.

### 3. The engine side adds an ingress, not a routing path

`StructureState` gains `control: Sender<ControlBatch>`, and `play` hands it a **clone of the very
sender its UDP decode thread holds**. Both producers feed one receiver, so a `send` and an external
datagram are indistinguishable downstream and there is no second routing path to keep in step. A
bare `Sender` suffices — the accept loop clones the state per connection rather than sharing it by
reference, which is std's multi-producer idiom.

Not a trait: a control sink has exactly one behavior forever, and `RenderConfigPublisher` earns its
trait only because it has two genuinely different implementations.

### 4. Consequences in the sidecar

`EngineLink` collapses to the `StructureClient` alone — no `UdpSocket`, no `osc_addr`, no
`send_osc`. `send` joins every other engine tool on **act-then-map**, so the probe-first exception
disappears and the rule has none left. `engine_status` reports one endpoint; the engine's OSC-in
port is its foreign edge, which the sidecar neither dials nor owns.

The JSON→`ControlArg` conversion stays in the sidecar rather than typing the tool parameter as
`Vec<ControlArg>` directly, so a bad argument is the tool's own named error instead of an rmcp-layer
deserialization failure. It guards the **f32 narrowing** too, not just the JSON type: a magnitude
past f32 range saturates to infinity, serde_json writes a non-finite float as `null`, and no
`ControlArg` variant accepts `null` — so one bad argument would otherwise take the whole batch down
with an opaque "did not match any variant".

`DEFAULT_OSC_PORT` moves from `reuben_core::coordinator::wire` to `reuben_native::osc`. It lived in
core justified as the literal "the sidecar and engine must both agree on" — with the sidecar no
longer dialing it, there is no second end to drift from, `play` is its only consumer, and core
carries no network plumbing.

`osc.rs`/`rosc` stay in `reuben-native`, where `play` is now their only consumer — external
controllers and `osc_out`, which is their actual job.

The ingress is a **constructor parameter**, not a builder step. `render_config` is a builder step
because it has a working headless default; an unwired control ingress has none, so it could only ever
be a wiring mistake surfaced to a user at runtime. Taking it in `StructureState::from_coordinator`
makes that mistake a compile error.

## Consequences

**The thing being ratified is the reversal itself**, not the plumbing. The loopback authoring door
is now a *second control ingress* alongside external OSC. That is sound because routing converges in
core (`boundary::osc_in_arg` → `queue_osc`), so auditioning behaves identically either way — but it
is a real change to the two-plane model ADR-0044 set, so it is recorded rather than assumed.

Multi-client semantics are unchanged: concurrent `send`s remain last-write-wins per control, exactly
as two physical OSC controllers behave. Batching only makes each individual gesture indivisible; it
arbitrates nothing between clients, and nothing here adds lease or session machinery.

`send` is now *more* reliable than before — a dead engine surfaces as a refused connect rather than
a datagram vanishing into a silent port — and the ack means more than it used to. The tool
description says so explicitly, and equally says what it still does not mean.

Structure and control share one channel but not one lock: `handle_send` never touches the
Coordinator, so a `send` cannot contend with the single-writer structure lock or be starved by an
in-flight swap's bounded reclaim poll.
