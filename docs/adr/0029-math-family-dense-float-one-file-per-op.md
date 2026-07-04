# Math is a family of dense `Float` ops, one file per op; the `Number` core is retired

> **Superseded in part by [ADR-0033](0033-number-operator-contract-macro.md):** pointwise number
> ops are no longer hand-written one file per op — they are generated from one scalar fn by
> `number_operator_contract!`. The one-operator-per-module rule and the `Number`-core retirement
> stand.

## Context

The math-operator family ([ADR-0017](0017-playable-surface-and-control-domain.md)) is built
around a `Number` trait and a `signal_pointwise!` macro: each op's arithmetic is written once
against the trait, and per-domain "shells" (a Signal-buffer variant, future Message variants) are
generated from it so the domains cannot drift. The family lives in one file, `math.rs`.

Two things made that core a footgun, raised as issue #73:

- **The boundary is undefined and the names mislead.** The trait is named `Number` and the file
  `math`, which reads as "all arithmetic lives here." It doesn't — it's "everything generated from
  the shared binary-pointwise core." `power` (`x^exponent`, [ADR-0027](0027-envelope-emits-cv-and-curve-ops.md))
  is unambiguously math yet can't be emitted by `signal_pointwise!` (one signal input, a
  metadata-bearing second operand, asymmetry, a NaN clamp), so it is correctly its own file — but
  nothing documents *why*, and the next curve op (`logarithmic`, …) is a coin-flip.
- **The core's premises were dissolved by [ADR-0028](0028-one-input-shape.md).** The `Number`
  core existed to (a) abstract over numeric *types* (f32 / int Messages) and (b) keep the Message
  and Signal *domains* from drifting. After ADR-0028 there is **one runtime number type** (`Float`
  is f32; "a runtime integer is a rounded `Float` or an `Enum`"), and the param-vs-input split is
  gone (a materialized `Float` is a knob *and* a wire). There is nothing left for the trait to
  abstract over and no second domain to keep in sync.

#73 asked for "one way to define an operator that works on numbers." Post-ADR-0028 that is no
longer a *type-abstraction* problem (there is one type) — it is a *single authoring mechanism*
problem. This ADR settles it. Resolved in a grilling session (2026-06-25).

## Decision

### Every math op is a dense `Float`→`Float` operator, one per file

`math.rs` is **deleted**. `add`, `mul`, `map`, `differentiate`, `integrate` each move to their own
file (`add.rs`, …) alongside `power.rs`, authored the standard way: `operator_contract!` + a
hand-written `process` + the op's own `register_operator!` line. The "is it a math op?" boundary
disappears by deletion — every op is a file, and its `shape` says which family it is in. The
`Number` trait and `signal_pointwise!` macro are **removed**.

`add`, `mul`, `power`, `differentiate`, `integrate` are dense `Float`→`Float` today. `map` moves to
its own file but **stays event-domain for now** — see its section below.

### All numeric operands are materialized `Float` with the identity as the declared default

No operand is a bare `signal` with an in-process fallback. `add`'s `a`/`b` default `0`, `mul`'s
default `1`, `power`'s `x` defaults `0` and `exponent` `2`. The engine materializes a full
`frames`-length buffer for any unwired operand (filled from the latched default), so:

- **"wire one side ⇒ passthrough the other" becomes data** (a declared default), surfaced in the
  descriptor — not just an `unwrap_or` constant buried in the loop.
- Every operand is **settable** (a constant offset/gain with no separate const op), is **always a
  full buffer in production** (the engine materializes the latched default when unwired), and
  surfaces in `settable_inputs()` for good-button + schema.
- The materialized full-buffer guarantee is what *permits* a branchless, vectorizable inner loop.
  The house convention (filter, pan, power) still writes the defensive
  `io.signal(IN).get(i).copied().unwrap_or(default)` so the same op unit-tests with a bare `None`
  input; the `unwrap_or` fallback **equals the declared default**, so the descriptor and the loop
  never disagree, and in production (full buffer) the fallback never fires. An operand that is
  genuinely block-constant can be const-folded via the `varying` hint and read once with
  `io.value`. (The substantive win of materialized-vs-bare is the descriptor surface above, not a
  change to the loop form.)

### The scalar math is a pure fn; the dense buffer is a shell over it

Each op's arithmetic is a tiny module-level fn — `fn add(a, b)`, `fn remap(v, …)`,
`fn step(prev, cur)` — and `process` is the **dense buffer shell** that calls it. This is the
[ADR-0017](0017-playable-surface-and-control-domain.md) "write the math once" instinct kept for
the **right reason**: not type abstraction (gone), but **carrier reuse** — a future sparse-`Float`
or `Note`-field shell (issue #83) reuses the same fn, making carrier overload *additive* rather
than a re-derivation.

A shared `pointwise2(io, …, fn)` helper for the symmetric two-input ops is **deferred** until a
third such op (`sub`/`min`/`max`) lands — at two call sites (`add`, `mul`) the signature would be
guessed against too few examples. Until then the two identical ~10-line loops are cheap insurance
against a wrong abstraction. The helper is an opt-in *call*, never a template an op is trapped in,
so an op with a bespoke loop (SIMD, a guard) simply doesn't call it.

### Calculus ops are dense, with a constant one-sample `dt`

`differentiate` and `integrate` become dense `Float`→`Float` ops with **`dt` = one audio sample,
unscaled**: `differentiate` is `out[i] = buf[i] − buf[i-1]` (carrying one sample of state across
blocks; the first ever sample seeds `last = buf[0]` so there is no startup spike), `integrate` is
a running Riemann sum `acc += buf[i]`. A **constant** `dt` is what makes higher-order calculus
valid — differentiate twice is acceleration only if the sampling window does not vary, which an
irregular sparse Δt cannot guarantee. Conversion to other time bases ("change per second", "per
beat") is a **separate, deferred** op, not baked in here. Gesture velocity is recovered by
materializing the gesture first (`m2s`/slew → dense CV) and then differentiating it.

### `map` moves to its own file but stays event-domain (its reframe defers with the instrument migration)

`map` is the target shape of this family — a dense `Float`→`Float` pointwise shaper — but it does
**not** convert here. Five bundled instruments (`good-button`, `strum-harp`, `chord-player`,
`djfilter-demo`, `groovebox`) wire `map` as a **Message** node today, and the codebase already
stages its "Float shaper reframe" to land **with the instrument migration** (the same staging
`m2s` follows). Converting it now would break those instruments and pull the whole message→`Float`
instrument migration into this change. So `map` moves to `map.rs` **unchanged** (event-domain `in`/
`out`, the emit-on-init resting value of ADR-0018 intact) — satisfying one-op-per-file and the
`math.rs` deletion — and its reframe (resting value folding into the `in` input's materialized
default) is deferred. Its affine math is already the module-level `remap` fn, so the reframe is a
shell swap, not a re-derivation.

### `power` is the curve-op precedent, and its old fork is gone

The param-vs-input fork [ADR-0027](0027-envelope-emits-cv-and-curve-ops.md) reasoned about no
longer exists: `exponent` is a materialized `Float` (`0.0..=8.0`, default `2.0`) — it keeps its
range guard, default, and UI knob **and** is wire-able, read **block-rate** (`io.value`; the curve
shape is held for the call, audio-rate exponent modulation is not worth a per-sample `powf`). The
unipolar clamp (`x.max(0.0)`) is an **op-local** NaN guard living in `power`'s pure fn, inherited
by nobody. Future curve ops (`logarithmic`, …) follow this exact shape: a dense `Float` op, one
file, a metadata-bearing block-rate shaping operand, op-local guards.

### Carrier overload is explicitly out of scope (issue #83)

A single op working on a number wherever it lives — dense `Float`, **sparse `Float`**, or the
numeric fields inside a `Note` — is the parked follow-up, tracked in #83. The scalar-fn + shell
structure above is chosen precisely so that work is additive. #83 also records that ADR-0028's
rejection of a `shape × temporality` model has a known counterexample (timing-reading ops), which
the sparse-`Float` carrier will force a revisit of.

## Consequences

- **Supersedes** the math-family mechanism of
  [ADR-0017](0017-playable-surface-and-control-domain.md): the `Number` trait and the
  `signal_pointwise!` per-domain shell macro are retired. The *family* (`add`/`mul`/`map`/
  `differentiate`/`integrate`, plus `power` from ADR-0027) is retained; only its one-core /
  one-file generation mechanism changes.
- `math.rs` is deleted; the golden descriptor snapshot and the generated instrument schema are
  re-blessed (operand ports gain `ParamMeta` defaults; `power.x` becomes materialized).
- **Behavioral change in the calculus ops.** They were Message-domain, computing Δt between
  sparse distinct events (gesture velocity); they are now dense with a one-sample `dt`. Patches
  relying on the old sparse-velocity behavior re-create it with `m2s` → `differentiate`.
- `differentiate`/`integrate` stop being `Shape::Note` ops (their `in`/`out` are now `Float`).
  `map` keeps its `Note` `in`/`out` until the instrument migration. The mislabeling of generic
  value events as `Note` is *not* fixed here — it is part of #83.
- Authoring docs (`docs/agents/authoring.md`), `ARCHITECTURE`, and module docs are swept to
  describe the math family as "dense `Float` ops, one file each, scalar-fn + dense shell," with
  `power` as the curve-op template.
