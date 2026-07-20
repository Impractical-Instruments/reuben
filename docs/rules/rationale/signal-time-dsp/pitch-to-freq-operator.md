# Why: `pitch2freq` is the single wire operator that exits the symbolic pitch domain — a pure `pitch`+`harmony` to held-`freq` lookup wrapping `Harmony::hz`, with glide, gating, and velocity left to downstream operators.

[Rule](../../signal-time-dsp.md#pitch-to-freq-operator)

The **Voicer was the only operator that turned a symbolic pitch into an output frequency** — it
resolved each held pitch through `Harmony::hz` and pushed the Hz onto the hosted voice's `freq` pipe,
the lowering welded inside the monolith next to note-priority, the latch, and voice allocation.
Everything else that touches pitch keeps it symbolic (`snap`, `chord` are Note→Note re-spellings that
never leave the Note domain). So a top-level mono voice (osc + env, no Voicer) had **no way to turn
its symbolic pitch into an oscillator frequency**, even though `Harmony::hz` already does the math —
`Degree` through scale+tuning (so it re-spells live on `/key`/`/mode`), `Absolute` through 12-TET. It
simply had no wire-exposed form. `pitch2freq` gives it one: a hand-written `operator_contract!`
operator (its inputs are vocab types, not numbers, so not a `number_op`) whose `process` is one line,
`freq = harmony.hz(pitch)`. Both inputs default sensibly (`Degree(0)`, `Harmony::DEFAULT`), so an
unwired op resolves to the tonic rather than faulting. This is the [context-owned
resolution](context-owns-resolution.md) reached over a wire — `pitch2freq` *calls* `harmony.hz`, it
never re-implements the chain.

Three boundaries keep it a pure lowering. **Pitch-only:** velocity and gate never enter it — in the
unbundling chain the latched `velocity` from [`unpack_note`](../composition-operators/product-type-unpack-operators.md)
flows on its own wire and *is* the gate. Folding velocity or a gate in would re-bundle exactly what
was dissolved, give the operator a second responsibility, and kill its reuse as a pure lowering (a
single `note → freq, gate` op was the monolith this map rejected). **Output is a held `Value`, not a
Signal:** `freq` is piecewise-constant, changing only on a new pitch or a `/key`/`/mode` re-spell, so
a Signal would recompute a constant every frame for nothing; the oscillator's `freq` Signal input
accepts the Value through the standard ZOH bridge, mirroring how the Voicer already drives `harmony.hz`
as a sparse change. **Glide stays downstream:** an `m2s` in Glide mode smooths the held `freq` into a
Signal (the 303 slide), so portamento is opt-in one node later and `pitch2freq` stays a pure lookup.
The name follows reuben's `X2Y` cross-domain-lowering form (`m2s`) and names both endpoints in the
wire's own vocabulary; `resolve` was rejected as naming neither endpoint, `pitch_hz`/`degree_to_freq`
because `hz` isn't the wire term and `Absolute` pitches aren't degrees. The Voicer is not yet
refactored onto this shared path — that unbundling is a later effort.

Distilled from: ADR-0064
