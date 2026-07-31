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

**A child source that builds completely is built once per load, and every further reference is a
fresh-state copy of that build.** The cache lives in `LoadCtx` beside the parse and decode caches it
completes, keyed by the same canonical id.

Two conditions on what may enter it, and both exist to keep the **cycle guard** honest. The first is
obvious: a build is cached only after it returns, so a source that re-enters itself is caught while
building rather than cached first. The second is not, and was found by adversarial review after the
first version of this change shipped it wrong. A build that degraded on **availability** — some
reference beneath it answered *unavailable* — is not cached at all; each site re-attempts it exactly
as it did before the cache existed.

The reason is that a build which lost a reference describes *a moment*, not the child. Cache it and
the moment becomes the answer for the rest of the load. Concretely, with `A → B → A` and a resolver
that withholds `A` on its first read (a file appearing mid-`git checkout` under `FsResolver`): `B`
builds with its `A` edge dark, gets cached, and when `A` is later built its reference to `B` is
answered from the cache — so `B` is never pushed onto the guard stack, and a cyclic library
dissolves into a silent instrument instead of a named `CyclicResource`. Refusing to cache such a
build restores the pre-cache behaviour exactly, and is the same policy the sample and document
caches already apply.

Only **instrument-kind** references count. A failed `sample` does not bar the cache: a sample is
not a graph, so it cannot re-enter the load and cannot hide a cycle, and its warning reaches every
site either way. Counting it would cost the cache on every reuse of a library child whose sample
the user has not installed — an ordinary case — and buy nothing.

### What this does not fix, stated plainly

Availability is not the only input a resolver can change underfoot. **Content** is too, and no
guard here notices: a resolver that serves different bytes for a source it already served leaves the
earlier build stale, and the same `A → B → A` shape then masks the same cycle. That is worth being
exact about, because a first draft of this ADR claimed otherwise.

It is not, however, new. `docs` has always assumed one canonical id names one document for a load,
so a `subpatch → subpatch` chain has behaved this way since long before any of this — verified by
running the content-mutating repro against the unmodified loader, which also returns a silent
instrument. What the built cache changes is that the assumption now holds on *every* path. The
voice path used to re-read by omission — it parsed without recording into `docs` — and so happened
to catch one shape the subpatch path already missed. That omission is closed here rather than
preserved: a source is read and parsed once per load however it is referenced, which is what `docs`
always claimed and only one caller delivered.

One visible consequence, under such a resolver only: two sites in one document naming one id can
now disagree, where a site that failed to read a source keeps its `ResolveFailed` while a sibling
splices the build a *different* reference obtained. Each site still reports what happened to it,
which is the per-site rule holding rather than bending.

**Both reference paths take the same short-circuit.** A source already in the cache is served
without consulting the resolver, whether the reference is a `subpatch` node or a voice copy. The
first version short-circuited only the voice path, which left a source first built for a voice pool
being re-read and re-parsed by a later `subpatch` reference purely to discard the result.

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
— one build and one source read for a pool of thirty-two, against thirty-two of each, provided the
voice document's own `patch`/`voice` references resolve. If one does not, that pool is back to
thirty-two builds by the rule above; a missing *sample* costs it nothing.

**A document with no repeated child is ~9% slower to load, and neither bench shape can see it.**
The first reference to a source pays one copy it would not otherwise need, so N references cost
about `1 + 0.09N` builds against the old `N` — break-even sits just above one reference. Measured
wall-clock, cache off → on: a single reference to a 64-node child 109 → 119 µs (+9%), 64 sites over
64 distinct children 1137 → 1250 µs (+10%), 32 sites over 32 distinct 24-node children 1419 → 1510
µs (+6%), against 64 sites over *one* child 1397 → 1006 µs (−28%). Peak RSS over a load of 300
distinct children rises ~12%, the resident templates.

This is not visible to the gate. `nest_doc` sweeps N sites over one `CELL_SOURCE` — the best case —
and `wide`/`deep` do not nest at all, so "the flat shapes are unchanged" is evidence about
non-nested documents and says nothing about a nested all-distinct one. It is recorded here rather
than benched because the trade is deliberate and one-directional: the documents that load slowly
enough to matter are the repetitive ones, and 9% of a small load is microseconds while the reuse
case is where the seconds are.

The alternative — deferring the cache until a source is referenced twice — was considered and
rejected. It makes a single reference free but costs a second build at two references, which is
worse than either arrangement for the shape immediately either side of break-even.

This lands against today's dissolve-only `subpatch`, while [ADR-0076](0076-a-swap-may-install-one-hosted-sub-plan.md)
has a Swap installing a hosted sub-`Plan`. The copy is at the `Graph` level, below where that split
is made, so hosting changes who asks for a built child rather than what a built child costs.
