# Pointwise number ops are generated from one scalar fn by `number_operator_contract!`

## Status

Accepted (2026-06-28). Supersedes in part [ADR-0029](0029-math-family-dense-float-one-file-per-op.md)
(hand-written one-file-per-op math).

## Context

[ADR-0029](0029-math-family-dense-float-one-file-per-op.md) made each math op a hand-written file
(`operator_contract!` + a `process` loop + `register_operator!`) and deferred a shared helper until a
third symmetric op appeared. [ADR-0031](0031-float-resolves-to-value-or-signal-by-wiring.md) then gave
each numeric op **two carriers** — a Signal form (per-sample buffers) and a Value form (held scalars,
`set` once) — selected by wiring. The result, raised as issue #104, was triplication: `add`, `mul`,
and `power` each carried a ~90-line `pub mod value { … }` that was near-identical boilerplate —
contract, empty struct, `new`/`spawn`, per-op test harness — differing only in the scalar op and the
operand defaults. `map`/`differentiate`/`integrate` got the `*F32Signal` rename but **no** value
variant, with no documented criterion for who gets one.

The scalar math was already a pure fn (ADR-0029's seam). What was duplicated was everything *around*
it. And ADR-0029's deferred-helper bet had come due: with `add`, `mul`, `power`, `map` all sharing the
same shape (read each operand, call a scalar fn, write the output), and a stated future of other
number types (`i32`, …), the right abstraction is no longer a `pointwise2` call helper but a
**declaration macro** that emits the whole carrier family. Resolved in a grilling session (2026-06-28).

## Decision

### One declaration emits the whole `numbers × carriers` family

`number_operator_contract!` takes a base name, the number type(s) the op supports, the carriers to
emit, an operand list, and a scalar-fn call-shape:

```rust
crate::number_operator_contract!(Add {
    numbers:  [f32],                 // future: [f32, i32]
    carriers: [value, signal],       // omit one for a single-form op
    inputs:   { a: number { default 0.0 }, b: number { default 0.0 } },
    outputs:  { out },
    function: add_fn(a, b),
});
```

For each `numbers × carriers` pair it emits a **submodule** (isolating the `IN_`/`OUT_` consts) with
the contract (via the same `render_contract` path as `operator_contract!`), an empty stateless
struct, the `Operator` impl whose `process` reads each operand per the carrier and calls the scalar
fn, `register_operator!`, and a contract-derived `defaults_are_data` test; the struct is re-exported
at the call site. `Add` over `[f32] × [value, signal]` yields `AddF32Value` (`add_f32_value`) and
`AddF32Signal` (`add_f32_signal`). The two axes are kept separate — `numbers` (the number type) and
`carriers` (value/signal) — rather than tangled in one list, so an op can be value-only or
signal-only by omitting a carrier, and a new number type is one `numbers` entry, not a doubling.

### The scalar fn is the only authored math; it is generic over the number type when it can be

The op writes one fn and names it in `function:` as a call-shape (`add_fn(a, b)`), which binds operand
names to arguments. A fn generic over `T` (`fn add_fn<T: Add<Output = T>>(a: T, b: T) -> T`) lets the
macro instantiate every `numbers` entry from it. An op whose math needs type-specific operations
(`power`'s `f32::max`/`powf` NaN guard) writes a concrete `f32` fn and lists `numbers: [f32]` only —
the f32-only restriction falls out of the fn's own signature, not a macro flag.

### Operand kinds: number follows the carrier, enum is always held

A `number` operand is a per-sample buffer in the Signal carrier and a held scalar in the Value
carrier — **uniformly**, with no per-operand rate. An `enum(VocabType)` operand is **always held** in
both carriers (enums have no buffer form). A number operand's `default` is optional (falling back to
the number type's zero) and its `range` is optional (falling back to the type-wide `±1e6`); the output
is named only and follows the carrier. Because `default` defaults to zero, a non-additive identity
must be stated (`mul`'s `default 1.0`); the generated `defaults_are_data` test pins it, so a forgotten
identity fails a test rather than silently zeroing patches.

The uniform rule **reshapes** `power`: its `exponent`, formerly a held block-rate `f32` read once, is
now a per-sample buffer operand in the Signal form. This is functionally identical for a held control
(block-slicing re-runs `process` on change) and drops the bespoke block-rate read; the only descriptor
change is `power_f32_signal.exponent` going `f32 → f32_buffer`.

### Eligibility criterion: stateless pointwise with number/enum operands

An op is macro-eligible — and gets both carriers — **iff** it is *stateless pointwise* (an output
sample is a function of this sample's inputs only) **and** every operand is a number or a held enum
mode. This is the criterion #104 found missing:

- **`add`, `mul`, `power`, `map`** qualify and are generated. `map` is **folded in** (its `MapCurve`
  is the enum-operand case) and renamed `map → map_f32_signal`, gaining a `map_f32_value`.
- **`differentiate`, `integrate`** are **stateful** (they carry `last`/`acc` across blocks), so they
  stay hand-written and **signal-only**. A value form would re-run `process` per change and shatter
  the continuous one-sample-`dt` stream / running accumulator. Statefulness, not a coin-flip, is why
  they have no value variant.

## Consequences

- **Supersedes** ADR-0029's hand-written-per-file authoring *for the pointwise number ops* and its
  deferred-`pointwise2` note: `add`/`mul`/`power`/`map` are now one macro call each over their scalar
  fn. The scalar-fn seam (issue #83 carrier reuse) is retained — it is exactly what the macro calls.
- **Resolves** ADR-0029's deferred `map` reframe: `map` is now a dense `Float` op in the family, the
  `==Exponential` test moved inside `remap` so `curve` passes through as the enum operand.
- A shared `operators/math_test.rs` (value/signal drivers + the F32 emit extractor) replaces the
  per-op test harness; each op's test file carries only its math assertions. The defaults test is
  macro-emitted.
- **Rename blast radius:** `map → map_f32_signal` (+ new `map_f32_value`) swept the 4 in-repo
  instruments, the native CLI test, a `format.rs` inline test, and the micro-bench
  `WORKLOADS`/`MICRO_IAI_KINDS` lists + `micro_iai.rs` attrs (both map variants now benched). `add`,
  `mul`, `power` keep their port names and type_names, so their descriptors are byte-identical (the
  golden confirms it) — no instrument or schema churn beyond `power.exponent`'s form.
- The golden descriptor snapshot is re-blessed (map rename + reshaped bounds, new value form,
  `power.exponent` form); the generated schema gains `map_f32_signal`/`map_f32_value` and drops `map`.
- Multi-type support (`i32`, …) is **designed for, not built**: the `numbers` axis and generic scalar
  fns are the seam, but only `f32` is emitted today. `F32` stays in the struct/type names to
  disambiguate the future types.
- Authoring docs (`docs/agents/authoring.md`) gain the `number_operator_contract!` path with the
  eligibility criterion; ops outside it (stateful, or with non-number/enum operands) stay on
  `operator_contract!`.
