# ADR-0075 — hosting splits on activity too, so the Voicer is not the only host

**Overturns** [`composition-operators.md#nesting-inline-or-host`](../rules/composition-operators.md#nesting-inline-or-host),
whose closing clause — *"the Voicer's polyphony being the sole host"* — is what this widens. The rule
is marked pending absorption in the same change, along with the `## Now` prose and the `Voicer`
`## Terms` entry that restate it.

## Context

Composition nests two ways, split on **cardinality**. A nested instrument with a fixed build-time
count is referenced by a `subpatch` node and **inlined** — its nodes splice into the parent's flat
schedule, boundary wires rewire to the inner targets, the node dissolves, and the runtime cost is
zero. Runtime-varying cardinality is **hosted**: the Voicer builds N standalone voice patches, turns
each into a sub-`Plan` with its own arena at `on_instantiate`, drives their interface inputs with
frame-stamped Messages, renders the active ones, and sums in fixed index order.

The split reads as *fixed count inlines, varying count hosts*, and the rule names the Voicer as the
sole host because polyphony was the only varying count anyone had.

A consumer of this engine now needs a third case the split has no slot for: **a fixed set of nested
instruments of which one is live at a time, changing while the graph plays.**

The shape is an arrangement primitive — a set of part-generator instruments per player, one live per
player, switching at musical boundaries. The count is known at build. What varies is *which member
is running*.

Under the current split that case must inline, which makes "which one is live" unchangeable without
a Swap — so every switch rebuilds the whole Engine and fires the declick ramp, for a transition that
should be a Message landing at a frame. A gain duck once per instrument reshape is correct. The same
duck once per chorus is not a cost, it is a defect.

Nothing about the *mechanism* was missing. The Voicer already renders a chosen subset of resident
sub-`Plan`s, re-entrantly, deterministically, and without allocating. Only the rule that decides who
may do that was too narrow.

## Decision

**The inline-or-host split widens from cardinality to whether the live set is decided at build.**

- A nested instrument whose membership **and** activity are fixed at build is **inlined and
  dissolved**, exactly as today. This remains the default and the cheap path.
- A nested instrument whose **live subset varies at runtime** is **hosted** as a live sub-`Plan` —
  whether what varies is the count (polyphony) or the selection (one of a fixed set).

**The Voicer becomes one host rather than the host.** A host is any operator that builds sub-`Plan`s
at `on_instantiate` over its own pre-allocated arenas, drives their interface inputs with a sparse
change-list of frame-stamped Messages, renders a chosen subset through the re-entrant `render_plan`,
and combines their outputs in fixed index order. Every existing obligation carries over unchanged:
allocation lives at instantiate, Render allocates nothing, and the combine order is fixed so output
stays bit-identical regardless of interleaving.

**A parked sub-`Plan` does not advance.** This is the same semantics the Voicer already has for an
inactive voice, and it is what makes residency affordable: the cost of a resident-but-parked member
is its memory, not its CPU. A host that wants a member to keep time while parked must render it, and
pay for it.

**What happens on entry is the host's declared policy, not an accident of the mechanism.** A member
that resumes where it left off and a member that restarts from the top are both legitimate, and a
host that supports either says which it does. The engine supplies the parking; it does not decide
what parking means musically.

## Consequences

- **The rule now splits on the right thing.** Inlining is for structure that is settled at build;
  hosting is for structure the graph chooses among while it runs. Cardinality was one instance of
  that, mistaken for the whole of it.
- **Residency is the price.** Every member of a hosted set is instantiated and holds its arena for
  the life of the Plan. Memory scales with the size of the set even though CPU scales with the live
  subset. A host over a large set is a large Plan, and nothing here bounds it.
- **A second host has to be written, not merely enabled.** The Voicer's selection logic is note
  allocation — assign, steal-oldest, release — which is a musical brain specific to polyphony. A
  selection host shares the sub-`Plan` machinery and none of that.
- **The mechanism deserves to be shared rather than re-implemented.** The Voicer currently owns its
  sub-`Plan` construction, arena allocation, interface driving and re-entrant render inline. A
  second host makes that a seam worth extracting; a third makes it mandatory. This ADR does not
  extract it, because one caller is not yet evidence of the right shape.
- **Determinism is unaffected.** A parked member that does not advance contributes nothing, and a
  live member combines in fixed index order. The existing guarantees hold without amendment.
- **The interface-pipe boundary does the work unchanged.** A hosted member is crossed by interface
  pipes like every other graph edge, which is what lets the host drive it without knowing anything
  about its interior.

## Open

- Whether the sub-`Plan` hosting machinery becomes a shared facility (a trait, or a helper the host
  operator composes) or stays duplicated until a third host exists.
- Whether a host may change the *size* of its resident set without a Swap. Nothing here permits it;
  a set that grows at runtime is a structural change and remains one.
- Whether a parked member should be able to opt into advancing — a musically-useful behaviour with a
  cost that defeats the reason parking is affordable. No caller needs it yet.
