# Why: The tonal-context node owns pitch resolution and snap as a deep module — degree to step via the symbolic Scale, step to Hz via the Tuning — so followers read Hz through io.context() rather than composing the chain themselves.

[Rule](../../signal-time-dsp.md#context-owns-resolution)

If every follower composed degree→step→Hz itself, each author would re-implement the chain and they
would drift — different snap, different rounding, subtle bugs, and lost AI-authorability. So the
context is a **deep module**: it exposes the resolver — `hz(pitch)`, `snap(pitch, policy)`,
`chord_tone(n)` — and the Scale∘Tuning composition lives in that one correct place. A follower stays
dumb, reading `io.context().hz(p)` exactly as it would read a param, which keeps single-lane authoring
simple and makes "always in key" a single shared implementation.

The representation is a two-stage pipeline: `degree --[Scale: degree→step]--> step --[Tuning: step→Hz]
--> Hz`. **Scale** is ordered step-offsets within the tuning's period plus a root (12-EDO major =
`[0,2,4,5,7,9,11]`); `degree d → root + scale[d mod len] + octave*period`. This is the load-bearing
split: swapping the Scala tuning changes Hz while the **degree structure is untouched**, preserving
the two-layer orthogonality ([two-layer-pitch](two-layer-pitch.md)). (A "major scale" is therefore not
universal across EDOs — inherent to microtonality, not a flaw.) Defining a scale in cents/ratios was
rejected because cents is frequency-space and would bypass the Tuning layer. **Chord** is a tagged
union — **scale-relative** (a set of scale degrees that *re-spells diatonically* as the key changes,
the feature) **or absolute** (raw step-offsets, *frozen* against key changes); the tag makes
"follows key" vs "frozen" an explicit call-site choice, defusing the silent-re-spell footgun, and there
is **one root authority** (the context root, no separate chord root). In the engine the resolver lives
on the `Copy` `Harmony` value in `vocab/harmony.rs`, so it snapshots onto the Message wire without
allocation, and `snap` is a separate concern layered on top ([pitch-snap](pitch-snap.md)).

Distilled from: ADR-0013
