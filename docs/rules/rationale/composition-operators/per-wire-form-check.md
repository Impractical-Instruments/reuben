# Why: The planner resolves forms with a local per-wire check whose only implicit coercions are Value→Signal materialization and `i32`→`f32` widening; every other crossing, including Signal→Value and `f32`→`i32`, is a hard error requiring an explicit converter operator.

[Rule](../../composition-operators.md#per-wire-form-check)

Because a port's form is declared, not propagated ([declared-port-forms](declared-port-forms.md)), the
planner needs no solver — only a **local per-wire check**: at each wire it compares the two declared
forms and does exactly one thing. Equal forms wire directly. The **one implicit form coercion** is
**Value→Signal**: a Value source into a `f32_buffer` sink ZOH-materializes the latched value into the
destination's block buffer at its change frame — a constant `cutoff` or a `voicer.freq` feeding an
`oscillator.freq`. Its reverse, **Signal→Value**, is a **hard error**: there is no honest implicit
sample-and-hold (*which* sample?), so crossing it needs an explicit sig→val converter (an envelope
follower, a quantizer) that does not ship yet — a deliberate, documented gap, not an oversight. Event
mismatches are likewise hard errors needing an explicit latch/change-detect op.

The second sanctioned coercion is the **`i32`→`f32` (and `i32`→`f32_buffer`) numeric widening**. The
justification is that it is *not a shape crossing*: both are the numeric wiring class, both (for
`i32`→`f32`) the same Value form, and the coercion is **total and lossless** — every `i32` in a
control range is a distinct `f32`, and the read already goes through `Arg::as_f32`. An explicit
`int_to_float` node would be pure boilerplate on every integer-control patch. It is **directional**,
mirroring Value→Signal exactly: `f32`→`i32` stays rejected because it forces a rounding *decision*
(the quantizer op that would bridge it does not ship yet, same as the envelope follower). This lets an
operator keep its modulatable `f32` ports while a *control* is honestly integer — euclid's
`steps`/`pulses`/`rotation` are `i32` controls widening into euclid's unchanged `f32` ports; the `f32`
is the transport, the `i32` is the meaning.

Two properties keep the check honest. It is **local, no propagation** — one arm in the pass-2 wire
check ([format/mod.rs](../../../../crates/reuben-core/src/format/mod.rs)), in the spirit of the
declared-form model; the widening is *not* a `same_wire_type` equality (i32 and f32 are distinct wire
types), so the lossy reverse simply has no path. And it is **rejected at load, in boundary terms**: a
mistyped wire into a nested boundary fails at load named as `/sub.audio`, not later at instantiate as a
`FormMismatch` on prefixed internals ([nesting-inline-or-host](nesting-inline-or-host.md)). Buffer
allocation falls straight out — allocate an `f32_buffer` only for a declared-Signal port or a
materialized Value→Signal edge; Value ports get a latch slot only.

Distilled from: ADR-0031, ADR-0061
