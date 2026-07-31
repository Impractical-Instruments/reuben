# ADR-0078 — a reused child document is built once and copied, which puts `Operator::spawn` on the load path

Cites [composition-operators](../rules/composition-operators.md) and
[web-product-process](../rules/web-product-process.md) for context. It overturns no rule: a reuse
still has "its own identity and state per use" — that is now maintained by an explicit copy rather
than falling out of a second build.

## Context

A load already deduplicated the *cheap* half of reuse and explicitly declined the expensive half.
Per load, a child source was fetched and decoded once, and parsed once, however many `subpatch`
nodes named it — but **built** once per reference. The comment that said so gave the reason: `Graph`
is not `Clone`, so two reuses got disjoint nodes, addresses and state for free.

Free is the wrong word once the reuse count stops being incidental. A song is a closure of
references — the same player named from many sections, every section resident — so a large document
is not N distinct things. It is a few hundred distinct things built over and over. The voice pass
was worse than the subpatch pass and in a way that never showed up in a bench: it consulted no cache
at all, so a 32-voice pool **read, parsed and built** the same source thirty-two times.

The alternative considered and set aside was a cached binary image of a built child. It is a strictly
larger change for the same win: the key would have to include the engine build id (cold exactly when
someone is iterating), the slow case is agent-generated or large-song structure that is unknowable
ahead of time, and it would mean serializing live operator state and pointer-linked buffers.

## Decision

**A child source is built once per load, and every further reference is a fresh-state copy of that
build.** The cache lives in `LoadCtx` beside the parse and decode caches it completes, keyed by the
same canonical id, and is populated only *after* a build returns — which is what keeps the cycle
guard intact, since a source that re-enters itself is caught during that first build and so is never
cached to be served past the guard later.

The copy is `Graph::spawn_copy`: same nodes, wires, taps, boundary and author overrides, with every
operator box taken through **`Operator::spawn`** and every stored `NodeKey` remapped through the
insertion rather than carried (SlotMap keys are per-map, so a carried key is not stale but
wrong-and-live).

**The consequence worth writing down is what this does to `spawn`.** It was a Voice-copy convenience
with one documented obligation — carry a decoded-sample binding forward. It is now the mechanism by
which every repeated node in every document comes into existence, and the obligation generalizes:
*any* binding must survive the call. The Voicer's `bind_voices` graphs are such a binding, and its
`spawn` dropped them, so the first copy of any section hosting a voice pool would have rendered
silence. That is the shape of the whole risk: a `spawn` that drops a binding is not a missed
optimization, it is a silent musical defect in every reference after the first, and it fails at
render time far from the trait impl that caused it.

The four other properties reuse already had are preserved deliberately, not incidentally:

- **Independent state** — `spawn` resets running state, so a copy starts silent on silence rather
  than replaying its template's charge.
- **Shared sample data** — `spawn` carries the `Arc`, so N copies of a sample-heavy child point at
  one decoded buffer instead of duplicating audio. A deep clone would have made memory *worse*.
- **Re-stamped addresses** — the splice prefixes each copy's addresses as before, so reuses still
  claim disjoint names and a post-prefix collision is still fatal.
- **Per-site warnings** — the cached build's warnings are *cloned* per referencing site and
  re-labelled with that site's address. An author who sees one warning for four broken nests has
  been told a quarter of the truth, so collapsing N builds must not collapse N diagnostics.

## Consequences

On the `nest` bench shape — N `subpatch` nodes over one child — construction costs ~13% fewer
instructions at every size, and the flat `wide`/`deep` shapes are unchanged to within 0.01%, which
is the controlled before/after the shape was added to produce. That figure is a floor set by the
bench's child, which is a single gain node behind two pipes: re-run with a sixteen-node child the
same measurement gives ~28% (×1.39), because what is saved is proportional to the size of the thing
no longer rebuilt. The voice pass is not on the bench at all and is the larger win in absolute terms
— one build and one source read for a pool of thirty-two, against thirty-two of each.

The first reference to a source pays one copy it would not otherwise need, and the template stays
resident for the rest of the load. Both are deliberate: sparing the copy would mean knowing the
reuse count before the first build, and the resident template is one child's worth of graph against
the reuses it replaces.

This lands against today's dissolve-only `subpatch`, while [ADR-0076](0076-a-swap-may-install-one-hosted-sub-plan.md)
has a Swap installing a hosted sub-`Plan`. The copy is at the `Graph` level, below where that split
is made, so hosting changes who asks for a built child rather than what a built child costs.
