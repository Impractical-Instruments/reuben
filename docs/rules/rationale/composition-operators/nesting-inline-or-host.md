# Why: A nested instrument with fixed build-time cardinality is inlined and dissolved into the parent's flat schedule for zero runtime cost, while runtime-varying cardinality is hosted as live sub-plans — the Voicer's polyphony being the sole host.

[Rule](../../composition-operators.md#nesting-inline-or-host)

Recursive composition ([recursive-composition](recursive-composition.md)) needs a nested instrument to
run *somewhere*, and there are two mechanisms, split by one clean line:

> **Fixed-at-build cardinality → inline (dissolve). Runtime-varying cardinality → host.**

**Inline** is the general, static case. A `subpatch` node names a sub-instrument (an
instrument-resource via a `patch` slot); at build its child nodes are spliced into the parent graph,
their addresses namespace-prefixed, every parent wire targeting a boundary pipe rewired straight to
the inner target, and the `subpatch` node **dissolves** — no sub-`Plan`, no sub-arena, no re-entrant
render call ever reaches the runtime ([subpatch.rs](../../../../crates/reuben-core/src/operators/subpatch.rs)).
This is the literal realization of "zero runtime cost." Two facts make the scary part a non-event:
connections are resolved to `NodeKey`s at build, not to addresses, so prefixing addresses on inline is
a pure naming transform over already-resolved edges; and the prefix is exactly the per-reuse identity
recursion requires — two uses (`/reverb`, `/reverb2`) get disjoint address sets and disjoint state
automatically. Encapsulation is a **namespace, not a seal**: prefixed internals stay OSC-reachable
(debugging, automation), matching the flat-graph reality that after dissolve they genuinely are
first-class parent nodes.

**Host** is the exception, and the Voicer is its **sole** instance. Voices *come and go*: idle ones
are skipped, stealing reassigns them mid-block — a fixed build-time splice cannot express "render only
the active voices this block." So the Voicer builds N standalone voice patches, keeps each as its own
`Plan` + arena, and calls the re-entrant `render(plan, arena, frames)` per *active* voice
([voicer.rs](../../../../crates/reuben-core/src/operators/voicer.rs)). Hosting is strictly worse than
inline where it isn't needed (per-instance arena + render-call overhead on the common static case that
has none of the dynamism), and inline is strictly worse than hosting for voices — so the two coexist
by design; neither is a precedent for the other.

Two structural payoffs keep the design cheap. Cross-boundary type-checking is **not a new checker**:
the synthesized boundary face presents each pipe with its declared `Arg` type
([interface-pipes](interface-pipes.md)), so the existing pass-2 wire check covers boundary wires
unchanged, and inlining then rewires to an inner edge that already passed the same check inside the
child. And **no node `id`** distinct from `address` is minted — the interface name is the stable
cross-document handle and namespacing supplies per-reuse identity, so `id` would be a second, redundant
indirection. Cyclic patch references are fatal (`CyclicResource`, keyed on the source string); diamond
reuse is legal.

Distilled from: ADR-0034, ADR-0032
