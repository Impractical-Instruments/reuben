# Why: Decoded resource bytes live in a central Coordinator-built ResourceStore, are referenced by logical id from a top-level resources table, and are read on the RT path through one pure accessor.

[Rule](../../authoring-library.md#resource-store)

The sample player is the first operator that is not a pure function of params + edges: it depends
on **external bytes** — an audio file that must be resolved, decoded, and held in memory before
render. Three standing contracts made that awkward, and the design reconciled them rather than
bolting on a special case. Construction is zero-arg and type-erased (`fn() -> Box<dyn Operator>`),
so there is no constructor slot for a decoded buffer; params are `f32`-only, so a sample *reference*
is a string; and `process` is allocation-free on the audio thread, so decoding a WAV — none of
those things — cannot happen at render. Decoded audio therefore does not live on the operator's
construction path at all.

Instead it lives in a central **`ResourceStore`** the Coordinator builds at load time (the single
writer of structure) and Render reads **immutably** — the same Coordinator-builds / Render-reads
split the runtime already enforces. A node refers to a resource by a **logical id** in a top-level
`resources` table (`"kick": "samples/kick.wav"`), not by an inline path: the indirection is the one
list the loader resolves+decodes, the natural **dedup** point (an id decoded once, shared by every
node referencing it), and the home the library/versioning thread grew into. Per-node path strings
(scatter the library concern) and inline base64 blobs (bloat, unreadable diffs, doesn't scale) were
rejected.

The accessor is written as a **pure function of `(id, range)`** for one forward-looking reason: the
long-term goal is a streaming audio bank whose total decoded size can exceed RAM. v1 keeps every
resource resident forever and `read` returns a slice; a future bank consults a warm-block cache
behind the *same* signature, and the operator never re-plumbs. Rooting ownership in a central store
(rather than an operator-owned `Arc<SampleBuffer>`) costs ~one struct now and is exactly the seam
streaming needs — the obvious operator-owned choice would force the bank to re-plumb every reader.
Determinism survives streaming because `read(id, range)` must always return the same floats for the
same arguments: a bank that falls behind **underruns** (an xrun, already the device layer's concern)
rather than substituting silence, so streaming affects glitch-vs-no-glitch, never *values*. Data
reaches the type-erased operator through a generic two-phase-init hook —
`bind_resources(&mut self, &Arc<ResourceStore>, &ResolvedRefs)`, default no-op — the idiomatic Rust
pattern for a plugin registry once constructor injection is off the table (precedent: nih-plug
`initialize`, CLAP `activate`); the descriptor declares a named resource slot so the loader knows
which nodes need a ref and the hook stays generic (*resources*, not *samples*). Codecs and
filesystem IO stay out of the portable core: core defines the types and a `ResourceResolver` seam,
`reuben-native` fills it with a WAV decoder, so compressed formats and non-file sources drop in
behind the same trait without touching core.

Distilled from: ADR-0016
