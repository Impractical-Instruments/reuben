# Why: Operators emit Messages over statically-wired typed edges, delivered downstream in the same block in topological order through preallocated emit pools.

[Rule](../../execution-runtime.md#operator-message-emission)

Operators must be able to *emit* Messages, not just receive them: without it every operator output
is a Signal and note data is one-way — it enters from outside, the Voicer consumes it once into
`freq`/`gate`, and nothing downstream can operate on notes again. That dead-ends a step sequencer
(no polyphony, no transpose, no arp, no tonal-context snap) and makes the tonal-context bus
unbuildable. Operator-emitted Messages are the missing foundation, not a sequencer feature.

The transport is a **statically-wired, typed Message edge**, deliberately *not* address/wildcard
dispatch. Address routing as the internal primitive would make every emit carry a `String` address
and an O(nodes) match — allocation and cost on the audio hot path. Instead an emit is **addressless
and port-bound**: an operator writes `(frame, payload)` on one of its own statically-declared output
ports (the `Emit` carries just a `port` index, one `Copy` `Arg`, and a `frame` — no address field),
and the wire from that port to a downstream input port is an ordinary connection resolved **once at
Instantiate**, exactly like the Signal arena, with topological ordering + cycle detection seeing it
like any other edge. Addresses are a *boundary* concept — external OSC routes to a node/port by
address, then the value travels internally by connection, never by name — so no string is matched or
carried on the render thread. Addressed/wildcard dispatch is layered on top later for the boundary
and meta-effects; it is never the internal routing primitive under notes.

Delivery is **topo-ordered and intra-block**: when a node emits, the engine routes those Messages
into the pending-event lists of nodes *downstream* of it before they run, so a note emitted at
frame 37 reaches the Voicer at frame 37 in the same block, sample-accurate. Reaching an *upstream*
node (a feedback edge) requires an explicit unit delay — the same rule cycles already follow
([deterministic-render](deterministic-render.md)); forward edges need no delay. This is the one real
change to the Render loop: routing **interleaves with execution** instead of running once up front —
it must, because a node's emissions depend on its inputs, which depend on upstream emissions.
Realtime-safety is by construction: a per-block emit pool and per-node emit scratch are preallocated
and *cleared* (not freed) each block, an emitted payload is a `Copy` `Arg` bound to a port index (no
string, nothing heap), and delivered events stay zero-copy views onto either the external slice or
the emit pool ([render-is-allocation-free](render-is-allocation-free.md)).

(emit was first built with a node-local `&'static str` address matched into a routing table;
the unified `Message`/`Arg` model folded routing together and made internal Value/Event writes fully
addressless — dispatch is by wired port connection, addresses are boundary-only. The durable
position this rule fixes — operators emit on statically-wired typed edges, delivered same-block in
topo order through preallocated pools — is what survives.)

Distilled from: ADR-0014
