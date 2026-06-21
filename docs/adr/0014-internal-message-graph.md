# Internal message graph: operators emit Messages

## Context

[ADR-0001](0001-unified-block-graph-execution.md) defined two things that flow on edges:
Signals (audio-rate float buffers) and Messages (discrete, OSC-shaped, sample-accurate
payloads). [ADR-0011](0011-message-delivery-and-timing.md) names the operators that *reason
in events* — "sequencers, the Voicer, note logic" — receiving time-ordered `(offset,
payload)` lists. [ADR-0005](0005-osc-namespace-and-wildcards.md) makes wildcard Message
dispatch "first-class … internally as well as externally," with meta-effects falling out of
internal dispatch "for free."

All of that presumes Messages move *between operators inside the graph*. The MVP never built
it. `render::route_messages` routes only **external** block-input Messages to nodes by
address; an operator can read its routed events (`io.events()`) but has **no way to emit**.
Every operator output is therefore a Signal, and note data is one-way: it enters from
outside, the Voicer consumes it once into `freq`/`gate` Signals, and nothing downstream can
operate on notes again.

This bit immediately. A step **sequencer** wants to be a note *source* feeding the Voicer
(polyphony, voice-stealing), a transposer, an arpeggiator, or the tonal-context snap. With
Signal-only output it can only drive one mono oscillator+envelope directly — a dead end in
the note domain. The same gap blocks the tonal-context bus
([ADR-0013](0013-tonal-context-bus-mechanics.md)), whose publishers and followers are
message ops. Operator-emitted Messages is the missing foundation, not a sequencer detail.

## Decision

**Operators can emit Messages. The transport is a statically-wired, typed Message edge — the
spine of the internal message graph. Address/wildcard dispatch (ADR-0005) layers on top of it
later; it is not the primitive.**

### Typed Message output ports, wired like Signal edges

The `Descriptor` already distinguishes `PortKind::Signal` from `PortKind::Message` and
already has Message *input* ports (the Voicer's `notes`, the Clock's `sync`). We make Message
*output* ports real and let an operator write to one:

```rust
io.emit(port, "note", [Arg::Float(midi), Arg::Float(vel)], frame);
```

- `port` selects a Message output port (a separate index space from Signal outputs, so the
  existing `io.output(signal_port)` numbering is untouched).
- the payload is a **node-local address** (`"note"`), typed args, and a **segment-relative
  frame**; the engine stamps it block-absolute. The local address is what the destination
  matches in `io.events()` — identical to how an external `/voicer/note` arrives as event
  `note`. On the wired hot path the address is a `&'static str`, so **emitting allocates
  nothing** (no `String`, args inline). The String-y wildcard path stays off this lane.
- Edges are ordinary `connections` (`sequencer.notes → voicer.notes`), resolved **once at
  Instantiate** into a routing table, exactly like the Signal arena. Message-kind ports take
  no arena buffer for their *data*, but topological ordering and cycle-detection see the
  connection like any other edge.

### Delivery is topo-ordered and intra-block (same-block, downstream-only)

Render already walks nodes in topological order. When node *N* emits, the engine routes those
Messages into the pending-event lists of nodes **downstream of *N*** before they run — so a
note emitted at frame 37 is delivered to the Voicer at frame 37 in the *same* block,
sample-accurate per ADR-0011. Reaching an *upstream* node (a feedback edge) requires an
explicit unit-delay — the same rule cycles already follow ([ADR-0009](0009-graph-lifecycle.md));
forward edges need no delay.

This is the one real change to the Render loop: routing **interleaves with execution**
instead of running once up front. After each node processes, its emit buffer is drained into
a block-lifetime message pool and the targets' event lists, so the existing block-slicing and
zero-copy event delivery carry emitted Messages unchanged.

### Realtime-safe by construction

- A per-block **emit pool** and per-node **emit scratch** are preallocated and cleared (not
  freed) each block — the same discipline as the edge arena ([ADR-0012](0012-boundary-and-threading.md)).
- Emitted addresses are `&'static str` and note-shaped args are inline, so a typical emit
  touches no allocator. Delivered events remain **zero-copy** views — now onto either the
  external Message slice or the emit pool.
- `tests/rt_safe.rs` is extended to cover a graph that emits (sequencer → voicer): steady
  state stays allocation-free.

### Lanes: emission is single-Lane (pre-fan-out)

A Message edge carries **one** stream, emitted before Voice fan-out — matching how a single
note stream feeds the Voicer, which then expands to Lanes ([ADR-0010](0010-single-lane-operators.md)).
Emission is honored from Lane 0 only; a multi-Lane operator emitting per-Voice Messages is
deferred (it has no consumer yet). The sequencer satisfies this: its Signal `clock` input is
single-Lane, so it is too.

## Considered and rejected

- **Address-routing as the primitive** (every emit re-enters `route_messages` by OSC
  address, matched against all nodes): maximally flexible and gives wildcards immediately,
  but every emit carries a `String` address and an O(nodes) match — allocation and cost on
  the audio hot path. The wired edge is the cheap common case; wildcard dispatch is built *on
  top* for the boundary and meta-effects, not underneath notes.
- **Keep Signals only, encode notes as CV** (the current state): no new machinery, but note
  data can never be re-processed — no polyphony from a sequencer, no transpose/arp/snap, and
  the tonal-context bus is unbuildable. A dead end, as the Signal-domain sequencer spike
  showed.
- **Route all emissions up front** (two-pass: collect every node's emissions, then deliver):
  can't work — a node's emissions depend on its inputs, which depend on upstream emissions.
  Delivery must interleave with topological execution.

## Consequences

- `Io` gains `emit`; `Descriptor` gains Message output ports; `Plan` gains a Message-edge
  routing table built at Instantiate; the Render loop interleaves routing with node
  execution. Signal-only operators are unaffected — they neither emit nor see new inputs.
- The **sequencer** is rebuilt as a note *source*: `clock` Signal in → `note` Messages out →
  `voicer.notes`. It becomes polyphony-, transpose-, and snap-composable. The Signal-domain
  version (PR #17) is superseded.
- The **tonal-context bus** (ADR-0013) now has its transport: publishers and the snap
  operator are message ops on this graph; the structured-value *latch* remains the only piece
  ADR-0013 still owns.
- **Wildcard / pattern dispatch** (ADR-0005) and **per-port event discrimination** (today
  events are delivered per-node and filtered by local address) are layered on later; this ADR
  builds the wired spine they extend.
