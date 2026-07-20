# Why: An operator reads and writes each port through a typed handle whose type fixes the port's form and carries its declared default, so a wrong-form read cannot compile and every declared Signal input is a dense buffer of exactly `frames` samples.

[Rule](../../composition-operators.md#typed-port-handles)

The single-source contract ([single-source-contract](single-source-contract.md)) bound a port's name
to its slot, but two seams between the declaration and each hand-written `process` stayed open. **The
type seam:** nothing bound a port's declared type to the payload type at the read site, so
`io.input::<Note>(IN_FREQ)` compiled even though `freq` is a Signal — silently returning an empty
stream that a finiteness-only test still passed. **Default duplication:** the descriptor declared
`default 440.0` and `process` restated it as `.unwrap_or(440.0)` — two sources that can drift, and
worse, since the latch is always seeded from the descriptor, the drifted literal is *misleading dead
code* that reads as the truth.

Both close with **typed handles**. `operator_contract!` emits one `In`/`Out` const per port whose
*type* is a form marker (`In<SignalF32>`, `In<Held<f32>>`, `In<Held<Waveform>>`, `Out<Event<Note>>`)
and whose value carries the declared default; `io.read`/`io.write` dispatch on the handle
([operator.rs](../../../../crates/reuben-core/src/operator.rs)). **S1 shuts** because the handle *is*
the declared form — there is no `usize` const left to feed a wrong-form read, so `io.read(IN_FREQ)`
cannot return an event stream, and a wrong-form read does not compile. **S2 shuts** because the held
read's fallback is the default the handle carries — one datum from the same contract tokens as the
descriptor, so `.unwrap_or(..)` disappears and no second literal can drift. (Defaults apply to held
reads only; a Signal read stays raw `&[f32]` — its old `.get(i).unwrap_or(0.0)` was a defensive length
guard, not a musical default.) The names are `In`/`Out` — not `InPort`/`OutPort` — because `CONTEXT.md`
lists "port" as a term to avoid, and the consts keep `IN_*`/`OUT_*` (an operator like `filter` has both
an `audio` input and an `audio` output, so prefix-less names would collide).

The enabling engine invariant, landed atomically with the handles, is **buffer-presence**: every
declared `f32_buffer` input handed to `process` is a dense buffer of exactly `frames` samples —
materialization is total over Signal inputs, and an unwired *bare* buffer fills with silence, so no
operator ever sees `&[]` or a short slice and `io.read(SIG)[i]` is safe by construction, the per-read
guards gone. Migration exposed and deleted real dead dual-form reads (`harmony` scanned per-sample
buffers its held inputs never had; `filter` re-read a materialized buffer's latch through a second
form). This is an authoring-surface change only — the descriptor, JSON schema, wire format, and
rendered output are all bit-identical.

Distilled from: ADR-0037
