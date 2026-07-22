# Why: A stateless pointwise number operator is declared once as a scalar function in `number_operator_contract!`, which generates its whole value/signal (and future number-type) operator family.

[Rule](../../composition-operators.md#pointwise-number-operators)

Once a numeric operator has both a Value form (held scalars) and a Signal form (per-sample buffers)
([declared-port-forms](declared-port-forms.md)), the two variants of `add`/`mul`/`power`/`map` are
near-identical boilerplate — contract, empty struct, `new`/`spawn`, a test harness — differing only in
the scalar op and the operand defaults. The scalar math was already a pure fn; what was duplicated was
everything *around* it, ~90 lines per carrier. With four ops sharing one shape (read each operand,
call a scalar fn, write the output) and a stated future of more number types (`i32`, …), the right
abstraction is a **declaration macro that emits the whole family**, not a call helper.

`number_operator_contract!` takes a base name, the number type(s), the carriers, an operand list, and
a scalar-fn call-shape, and for each `numbers × carriers` pair emits a submodule (isolating the
`IN_`/`OUT_` consts) with the contract, a stateless op carrier whose `ValueOp`/`SignalOp` impl names
those very consts as its handles, a `pub type` alias binding that carrier to its shell,
`register_operator!`, and a contract-derived `defaults_are_data` test
([number_op.rs](../../../../crates/reuben-macros/src/number_op.rs)).
`process` itself is **not** emitted: it belongs to the two shells
([shell.rs](../../../../crates/reuben-core/src/operator/shell.rs)), written once per carrier. Keeping
the per-sample loop in one place is what lets the signal shell hoist each operand's slice read out of
it, which the emitted-per-variant body could not do — the read sat inside the loop, costing a bounds
check per operand per sample and blocking vectorization.
`Add` over `[f32] × [value, signal]` yields `AddF32Value` and `AddF32Signal`. The **two axes are kept
separate** — `numbers` (the type) and `carriers` (value/signal) — so an op can be value-only or
signal-only by omitting a carrier, and a new number type is one entry, not a doubling. The **scalar fn
is the only authored math**: a fn generic over `T` lets the macro instantiate every number type from
it; an op whose math needs type-specific ops (`power`'s NaN guard) writes a concrete `f32` fn and
lists `[f32]` only — the restriction falls out of the fn's own signature, not a macro flag.

The eligibility criterion is the load-bearing boundary: an op is macro-eligible — and gets both
carriers — **iff it is stateless pointwise** (an output sample is a function of this sample's inputs
only) **and** every operand is a number or a held enum mode. `add`/`mul`/`power`/`map` qualify.
`differentiate`/`integrate` are **stateful** (they carry `last`/`acc` across blocks), so they stay
hand-written and signal-only: a value form would re-run `process` per change and shatter the
continuous one-sample-`dt` stream. Statefulness, not a coin-flip, is why they have no value variant —
the criterion is exactly the one that was missing when each math op was hand-written per file.

Distilled from: ADR-0033
