# Why: Operator, Instrument, and Rig are one recursive concept: an Instrument is a named subgraph that exposes an interface and is reused as if it were an operator, with its own identity and state per use.

[Rule](../../composition-operators.md#recursive-composition)

reuben has three scales of structure — Operators, Instruments, Rigs — and they could have been three
distinct types with separate code, schemas, and mental models. They are instead **one recursive
concept**: a graph of nodes with typed ports, at every scale. An Instrument is a named subgraph that
exposes boundary ports (its [interface pipes](interface-pipes.md)) and is usable inside another
Instrument or a Rig *as if it were an operator*; a Rig is simply the outermost graph. The single
biggest lever here is **AI-authorability**: an agent, or a person, learns one node model, one port
model, one connection rule, one file schema — from operator to rig — instead of three. Three layered
types would triple the implementation and cap composition at fixed layers.

Recursion is an **authoring** concept, not a runtime one. The two costs it could impose are both paid
off elsewhere. **Per-reuse identity and state isolation** — each reuse of an Instrument gets its own
operator identities and its own state, with no accidental sharing — falls out of the flat graph's
stable-key node identity: a node is a slotmap key ([graph.rs](../../../../crates/reuben-core/src/graph.rs)),
and on nesting each reuse's addresses are namespace-prefixed, so two uses yield disjoint address sets
and therefore disjoint state, automatically. **Runtime cost** is removed by how a nest is realized —
a static nest is inlined and dissolved into the one flat schedule, so recursion costs nothing at
render ([nesting-inline-or-host](nesting-inline-or-host.md)). Beginners are shielded from pathological
nesting by the Toy layer and good defaults, not by a hard cap.

The one deliberate divergence from the original conception: it assumed *all* nesting inlines at
plan-build. Polyphony needs instances that come and go at runtime, so the Voicer **hosts** its voice
sub-patches rather than inlining them — the split is now drawn on cardinality
([nesting-inline-or-host](nesting-inline-or-host.md)). The recursion itself — one concept at every
scale, reused-as-operator, own identity and state — is untouched.

Distilled from: ADR-0003
