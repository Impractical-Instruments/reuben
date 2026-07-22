# Why: A stateless pointwise number operator is declared once as a scalar function in `number_operator_contract!`, which generates its whole value/signal (and future number-type) operator family.

[Rule](../../composition-operators.md#pointwise-number-operators)

Once a numeric operator has both a Value form (held scalars) and a Signal form (per-sample buffers)
([declared-port-forms](declared-port-forms.md)), the two variants of `add`/`mul`/`power`/`map` are
near-identical boilerplate — contract, empty struct, `new`/`spawn`, a test harness — differing only in
the scalar op and the operand defaults. The scalar math was already a pure fn; what was duplicated was
everything *around* it, ~90 lines per carrier. With four ops sharing one shape (read each operand,
call a scalar fn, write the output) and a stated future of more number types (`i32`, …), the right
abstraction is a **declaration macro that emits the whole family**, not a call helper.

`number_operator_contract!` takes a base name, a `variants:` list, an operand list, and a scalar-fn
call-shape, and for each variant emits a submodule (isolating the `IN_`/`OUT_` consts) with the
contract, a stateless op carrier whose `ValueOp`/`SignalOp` impl names those very consts as its
handles, a `pub type` alias binding that carrier to its shell, `register_operator!`, and a
contract-derived `defaults_are_data` test
([number_op.rs](../../../../crates/reuben-macros/src/number_op.rs)).
`process` itself is **not** emitted: it belongs to the two shells
([shell.rs](../../../../crates/reuben-core/src/operator/shell.rs)), written once per carrier. Keeping
the per-sample loop in one place is what lets the signal shell hoist each operand's slice read out of
it, which the emitted-per-variant body could not do — the read sat inside the loop, costing a bounds
check per operand per sample and blocking vectorization.

Each `variants:` entry is `<number type> [-> <number type>] <carrier>` and names exactly one
operator: `Add` over `[f32 value, f32 signal, i32 value]` yields `AddF32Value`, `AddF32Signal`, and
`AddI32Value`. The optional arrow gives the **output** type where it differs from the input's — a
**converter**: `Round` over `[f32 value, f32 signal, f32 -> i32 value]` adds `RoundF32I32Value`
(`round_f32_i32_value`). Omitting the arrow means "out is in", so no op whose arithmetic stays in one
type writes one, and the out fragment appears in the name only where the types actually differ —
`add_f32_value` is not `add_f32_f32_value`, because the type name is the operator's identity on the
wire and restating it would migrate every instrument document.

It is a **written list rather than a `numbers × carriers` product** because the product is not full —
`i32` has no dense buffer form, so `i32 signal` does not exist and is rejected at the parse. A product
would need a per-number carrier table to say so, and could not name a converter at all: a converter
is not a cell of that grid but a *pair* of number types. Listing the instantiations says both
directly, and a missing entry is a missing operator rather than a silently-skipped cell. The
bufferless check reads both positions, which is one statement covering two facts — integer operators
are value-only, and so is every converter producing one.

One operand declaration serves every entry: a `default 1` is `1.0` in the `f32` instantiations and
`1` in the `i32` ones, and a value that cannot survive the projection (a fractional default on an op
that also lists `i32`) is a compile error at the operand.

The **scalar fn is the only authored math**, and it is what restricts an op to a subset of the number
types: a fn generic over `T` lets the macro instantiate every type from it; an op whose math is
type-specific (`power`'s `powf`) writes a concrete `f32` fn and lists only `f32` entries. Naming an
`i32` variant for such an op **fails to compile at the call site** — the restriction falls out of the
fn's own signature, not a macro flag. The same holds across the arrow: the rounding family's fns are
generic over their *output* type (`RoundInto<Out>`), so `f32 -> i32` compiles exactly because that
impl exists, and an unimplemented pairing is a missing-impl error rather than a wrongly-typed
operator. That is a real guarantee rather than a claim: it holds because
the declared number type reaches the ports and the instantiation, which it did not before issue #556,
when the type was consumed at the struct name alone and every generated port was `f32` regardless.

Generic bounds are chosen for **totality across the instantiated types**, not just for what compiles.
The five operations that can leave a type's range — add, sub, mul, neg, abs — are bound on
`PointwiseNum` ([pointwise.rs](../../../../crates/reuben-core/src/operators/pointwise.rs)) rather than
the corresponding `core::ops` traits, because `i32`'s operators panic on overflow in a debug build
where `f32`'s yield `inf`, and `process` runs on the render thread. `PointwiseNum` saturates at each
type's limits, making `inf` and `i32::MAX` the same answer in two types. The declared port range does
not substitute for this: every inbound value is clamped to it, but `mul` still escapes — two operands
at the type-wide `±1e6` sentinel multiply past `i32::MAX`.

Which *number types* an op lists is a separate judgment from eligibility, and only one of its failure
modes is mechanical. Bounds that reject the type are a compile error (`power` at `i32`). An op that
compiles but is semantically useless is the author's call, recorded in the module doc (`reciprocal`
at `i32`, where `1/n` is `0` for every `|n| > 1`). An op that compiles but needs a different
algorithm is a separate operator (`map`'s normalized fraction needs reassociating for integers). The
family ships every type an op can answer correctly rather than only the ones with a demonstrated
consumer: a partial family has to be probed, where a complete one can be learned.

The eligibility criterion is the load-bearing boundary: an op is macro-eligible — and gets both
carriers — **iff it is stateless pointwise** (an output sample is a function of this sample's inputs
only) **and** every operand is a number or a held enum mode. `add`/`mul`/`power`/`map` qualify.
`differentiate`/`integrate` are **stateful** (they carry `last`/`acc` across blocks), so they stay
hand-written and signal-only: a value form would re-run `process` per change and shatter the
continuous one-sample-`dt` stream. Statefulness, not a coin-flip, is why they have no value variant —
the criterion is exactly the one that was missing when each math op was hand-written per file.

Distilled from: ADR-0033
