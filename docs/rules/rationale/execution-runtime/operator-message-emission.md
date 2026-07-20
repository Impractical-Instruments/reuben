# Why: Operators emit Messages over statically-wired typed edges, delivered downstream in the same block in topological order through preallocated emit pools.

[Rule](../../execution-runtime.md#operator-message-emission)

Operators must be able to *emit* Messages, not just receive them: without it every operator output
is a Signal and note data is one-way — it enters from outside, the Voicer consumes it once into
`freq`/`gate`, and nothing downstream can operate on notes again. That dead-ends a step sequencer
(no polyphony, no transpose, no arp, no tonal-context snap) and makes the tonal-context bus
unbuildable. Operator-emitted Messages are the missing foundation, not a sequencer feature.

The transport is a **statically-wired, typed Message edge**, deliberately *not* address/wildcard
dispatch. Address routing as the primitive would make every emit carry a `String` address and an
O(nodes) match — allocation and cost on the audio hot path. Instead an emit names a node-local
address as a `&'static str` with inline args and a frame, so **emitting allocates nothing**; edges
are ordinary connections resolved **once at Instantiate** into a routing table, exactly like the
Signal arena, and topological ordering + cycle detection see them like any other edge. Wildcard
dispatch layers on top later for the boundary and meta-effects; it is not underneath notes.

Delivery is **topo-ordered and intra-block**: when a node emits, the engine routes those Messages
into the pending-event lists of nodes *downstream* of it before they run, so a note emitted at
frame 37 reaches the Voicer at frame 37 in the same block, sample-accurate. Reaching an *upstream*
node (a feedback edge) requires an explicit unit delay — the same rule cycles already follow
([deterministic-render](deterministic-render.md)); forward edges need no delay. This is the one real
change to the Render loop: routing **interleaves with execution** instead of running once up front —
it must, because a node's emissions depend on its inputs, which depend on upstream emissions.
Realtime-safety is by construction: a per-block emit pool and per-node emit scratch are preallocated
and *cleared* (not freed) each block, addresses are `&'static str`, args inline, and delivered
events stay zero-copy views onto either the external slice or the emit pool
([render-is-allocation-free](render-is-allocation-free.md)).

Distilled from: ADR-0014
