# Why: Every operator, port, and param is auto-addressable by its structural path through the graph, and an instrument additionally exposes a curated set of stable named addresses as its refactor-safe control surface.

[Rule](../../signal-time-dsp.md#structural-and-exposed-addressing)

Messages are OSC-shaped and composition is recursive, so something must define what an address
*names*. Two needs pull in opposite directions: an agent authoring a patch wants **zero-config,
predictable** addresses for everything, while an external controller mapping wants a **stable** target
that does not move when internals are refactored. The hybrid serves both. Every operator, port, and
param is auto-addressable by its **structural path** through the graph nesting
(`/lead-synth/filter/cutoff`) — nothing to declare, and predictable for a machine reading the tree.
On top of that an instrument **exposes a curated set of named addresses** — its public control
surface — and exposing a control is the same act as exposing a boundary port: publishing the
instrument's public API. External mappings bind to the exposed address and survive internal rewiring,
which is exactly what the structural path cannot promise (renaming or moving an operator changes its
path). Names must be unique within a parent scope so structural paths stay unambiguous.

**Wildcard/pattern dispatch is designed but not yet the internal primitive.** The original decision
imagined OSC pattern matching (`/drums/*/decay`) honored on *internal* Message dispatch too, so one
gesture fans across many targets and an effect "rack" falls out for free. That remains the intended
meta-effect mechanism at the boundary, but internally it is **not implemented**: internal edges are
statically wired and port-bound (an emit carries a port index, not an address), resolved once at
Instantiate — the cheap common case that keeps no string match on the audio thread. Wildcards layer on
later, at the boundary and for meta-effects; they are not live internal routing today. (See the
[execution-runtime](../../execution-runtime.md) topic's operator-message-emission rule for why the
internal transport is deliberately addressless.)

Distilled from: ADR-0005
