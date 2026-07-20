# ADR-0064: `pitch2freq` resolves a symbolic pitch to a frequency (de-trapping `harmony.hz`)

## Status

Accepted (2026-07-20). **Design locked, not yet implemented** — the implementation is a downstream
effort. Decided through wayfinder map [#517](https://github.com/Impractical-Instruments/reuben/issues/517),
ticket [#523](https://github.com/Impractical-Instruments/reuben/issues/523). Completes the map — the
last of its three primitives.

Depends on [ADR-0062](0062-payload-enums-are-first-class-arg-leaves.md) (leaf-promotion): `pitch2freq`
consumes a bare `Pitch` on the wire, which exists only once `Pitch` rides an `Arg` on its own. Sibling
of [ADR-0063](0063-product-vocab-types-unpack-to-fields.md) (make/break): `unpack_note`'s `pitch`
output is `pitch2freq`'s input — together they express the mono voice as `unpack_note` → `pitch2freq`
→ osc/env, the unbundling test ([#518](https://github.com/Impractical-Instruments/reuben/issues/518)).

## Context

The **Voicer is the only operator that turns a symbolic pitch into an output frequency.** It resolves
each held pitch through `Harmony::hz` (`vocab/harmony.rs`) and pushes the resulting Hz onto the hosted
voice's `freq` pipe — the lowering is welded inside the monolith, alongside note-priority, the latch,
and voice allocation. Everything else that touches pitch keeps it **symbolic**: `snap` and `chord`
are `note`→`note`/`degrees` harmonic re-spellings that never leave the Note domain; `harmony.hz` is
called only by `voicer`/`snap`/`chord`, but only the Voicer uses it to *emit a frequency*.

So a top-level mono voice (osc + env, no Voicer) has **no way to turn its sequencer-driven symbolic
pitch into an oscillator frequency**. `Harmony::hz` already does the math — `Degree` through
scale+tuning (so it re-spells live on `/key`/`/mode`), `Absolute` through 12-TET — it simply has no
wire-exposed form. This ADR gives it one.

## Decision

Add a wire operator that is the thin, wire-exposed form of `Harmony::hz`.

### 1. Scope: pitch-only

`pitch2freq` takes a `Pitch` and a `Harmony` and emits `freq` — nothing else. Velocity and gate never
enter it: in the unbundling chain they flow on the **separate** `velocity` wire out of `unpack_note`,
and per the success-test ([#518](https://github.com/Impractical-Instruments/reuben/issues/518)) that
latched velocity *is* the gate (the envelope retriggers off it). Folding velocity or a gate in would
re-bundle exactly what the map dissolved, give the operator a second responsibility, and break its
reusability as a pure lowering. A single `note → freq, gate` op was the monolith
[#515](https://github.com/Impractical-Instruments/reuben/issues/515) proposed and this map rejected.

### 2. Port list

A hand-written `operator_contract!` operator (like `snap`/`chord`), **not** a `number_op` — its inputs
are vocab types, not numbers.

```
pitch2freq
  inputs:  pitch:   Pitch   (Value, default Degree(0))       // from unpack_note.pitch (ADR-0063 baseline)
           harmony: Harmony (Value, default Harmony::DEFAULT)
  outputs: freq:    f32     (Value)
```

`process` is one line — `freq = harmony.hz(pitch)` — covering both `Degree` (scale+tuning) and
`Absolute` (12-TET). Both inputs default sensibly, so an unwired or partially-wired `pitch2freq`
resolves to the tonic frequency rather than faulting.

### 3. Output carrier: Value, not Signal

`freq` is emitted as a **held `Value`**, not a per-sample `Signal`:

- It is **piecewise-constant** — it changes only when a new pitch arrives or `Harmony` re-spells on a
  `/key`/`/mode` change. A Signal would recompute a constant every frame for nothing.
- The oscillator's `freq` **Signal** input accepts this Value through the **standard ZOH bridge**
  (ADR-0031); the coercion is automatic at the wire.
- It mirrors the Voicer, which already drives `harmony.hz` onto the voice `freq` pipe as a **sparse
  change** (Value semantics), not a stream.
- **Glide stays downstream.** An `m2s` in Glide mode takes this held `freq` and smooths it into a
  Signal (the 303 slide). `pitch2freq` stays a pure lookup; portamento is opt-in, one node later.

### 4. Naming: `pitch2freq`

`type_name = "pitch2freq"`, struct `Pitch2Freq` — the compressed `X2Y` form reuben already uses for a
cross-domain lowering (`m2s`). It names both endpoints in the wire's own vocabulary: the output port
is `freq` (Hz is only the unit and the `harmony.hz` method name), and `pitch` correctly umbrellas both
`Degree` and `Absolute`.

**`resolve` was rejected as too generic** — it names neither the input nor the output, and "resolve"
could later apply to other symbolic reductions. The descriptive endpoint-naming form disambiguates
against any future lowering. `pitch_hz`/`pitch2hz` were rejected because `hz` isn't the wire term;
`degree_to_freq` because it wrongly excludes `Absolute` pitches.

**Relationship to `snap`/`chord`:** those are Note→Note harmonic re-spellings that keep pitch
*symbolic*; `pitch2freq` sits downstream of all of them as the single stage that **exits** the
symbolic domain into raw Hz. No overlap.

## Consequences

- **The mono voice becomes a patch.** `unpack_note` (ADR-0063) → `pitch2freq` → osc/env expresses what
  the Voicer did monolithically, completing the unbundling test (#518). `harmony.hz` is de-trapped —
  no longer reachable only from inside the Voicer.
- **Symbolic re-spelling still works end to end.** Because a `Degree` resolves through the live
  `Harmony`, degree sequences plus `/key`/`/mode` changes re-spell exactly as under the Voicer; an
  `Absolute` pitch passes straight through 12-TET.
- **Hot path is trivial.** One `harmony.hz` call per change, on `Copy` inputs, emitting a Value —
  allocation-free, lighter than the Voicer's per-voice bookkeeping.
- **Depends on ADR-0062 landing first** — `pitch2freq`'s `pitch` input has no wire form until `Pitch`
  is leaf-promoted. Implementation order: leaf-promotion → { `unpack_note`, `pitch2freq` }.
- **The Voicer is not yet refactored onto this** — extracting the Voicer's internal lowering to call
  the shared path (or expressing the Voicer itself as a patch) is the later "unbundle the Voicer"
  effort, out of scope on map #517.
