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
`Harmony`, `Pitch`, `Enum`, and a whole `F32Buffer`: a vocabulary a control channel would
immediately have to forbid, which is the web codec's argument verbatim. Nor is `Arg` serializable
today, and making it so — hanging serde on the type the render thread passes around, for one
channel's benefit — is the wrong direction. How primitives are spelled on *this* NDJSON channel is
definitionally this channel's shape choice, which is what `wire` is for.

`ControlArg` is `#[serde(untagged)]`, so an atom rides as its bare JSON value (`[800.0, "up", 3]`).
Two reasons: the channel stays netcat-debuggable, which its framing exists to serve; and the
`I32`/`F32` split falls out of JSON's own integer-vs-float spelling rather than a rule each client
reimplements — the sidecar had hand-written exactly that policy. **Variant order is load-bearing**
(`I32` before `F32`), and is pinned by a test.

### 2. The verb is batched, not per-message

`Request::Send { messages: Vec<ControlMessage> }` → `Response::Sent { count }`.

The tool contract is already a batch — the authoring gesture is multi-control. Per-message would
mean N TCP connects and N spawned engine-side threads to replace one `UdpSocket::bind` that
dispatched N datagrams, and would split one gesture into N independently-failable steps that can
half-apply. One exchange makes the gesture atomic in flight — *stronger* than the UDP it replaces,
where `send_to` could already fail mid-batch — and keeps a concurrent client from interleaving into
the middle of it.

The ack is the engine's own count of what it **queued**, not what it applied: an address routing to
no node/port is dropped at the ingress, exactly as a stale external datagram is.

### 3. The engine side adds an ingress, not a routing path

`StructureState` gains `control: Option<Sender<OscIn>>`, and `play` hands it a **clone of the very
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

`DEFAULT_OSC_PORT` moves from `reuben_core::coordinator::wire` to `reuben_native::osc`. It lived in
core justified as the literal "the sidecar and engine must both agree on" — with the sidecar no
longer dialing it, there is no second end to drift from, `play` is its only consumer, and core
carries no network plumbing.

`osc.rs`/`rosc` stay in `reuben-native`, where `play` is now their only consumer — external
controllers and `osc_out`, which is their actual job.

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
