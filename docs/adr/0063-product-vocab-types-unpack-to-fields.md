# ADR-0063: Product vocab types unpack to their fields via generated `unpack_<type>` operators (make/break)

## Status

Accepted (2026-07-20). **Implemented** (2026-07-20,
[#539](https://github.com/Impractical-Instruments/reuben/pull/539)) — the `unpack_op!` census macro
and its first operator `unpack_note`, with `pitch` promoted to a first-class held-Value port form.
Decided through wayfinder map [#517](https://github.com/Impractical-Instruments/reuben/issues/517),
ticket [#522](https://github.com/Impractical-Instruments/reuben/issues/522).

Depends on [ADR-0062](0062-payload-enums-are-first-class-arg-leaves.md) (leaf-promotion): `unpack`
cannot emit a `Pitch` field onto the wire until `Pitch` rides an `Arg` on its own. Sibling of
**resolve** ([#523](https://github.com/Impractical-Instruments/reuben/issues/523)), which consumes
the `Pitch` this operator produces. The **`pack`** direction (constructing a product type from its
fields) is deferred to a follow-up issue — see *Deferred* below.

## Context

The map set out to dissolve *capability trapped inside monolithic operators*. The **Voicer** is the
worst offender: it bundles note-priority bookkeeping, the Event→Value latch of held pitch/gate,
harmony resolution, and voice allocation into one opaque box. The mono-voice unbundling test
([#518](https://github.com/Impractical-Instruments/reuben/issues/518)) wants that expressible as a
patch — `unpack` Note → `resolve` → osc/env — so the first job is a wire operator that turns a
`Note` **event** stream into its held fields.

Nothing in the registry does this. Everything that touches a `Note` keeps it whole (`sequencer`,
`transpose`, `snap`, `chord`); only the Voicer lowers it, and only internally. And a `Note` field
crossing from an **Event** stream to a **Value** an oscillator/envelope can read is exactly the
`Event → Value` boundary that `check_wire_forms` rejects without an explicit latch (ADR-0031) —
there is no such latch node.

Two forces shaped the decision:

- **The map's guiding principle** — *low-effort-to-extend, no per-type hacks*. Whatever gives one
  product type its field↔wire operators must give **every** product type theirs at near-zero
  marginal cost, the way `number_operator_contract!` already mints a whole family of pointwise ops
  from one declaration and `inventory` discovers them with no central match.
- **Prior art** ([#520](https://github.com/Impractical-Instruments/reuben/issues/520)) — the
  universal ecosystem verb pair for struct destructure/construct is **`pack`/`unpack`** (Max, PD,
  …); no environment uses "make/break". Field outputs are **ZOH-held** (reuben's native `Value`
  behavior); **last-note** priority is the default; and symbolic-pitch resolution is kept **out** of
  the destructure and put in a dedicated stage (→ resolve, #523).

## Decision

### 1. Scope: `unpack` only this effort; `pack` deferred

Generate the **`unpack`** (destructure) direction only. `pack` (construct a product type from field
inputs) is **not expressible today**: every product vocab type has a **sum-typed field that cannot
be constructed on the wire** — `Note.pitch` is a `Pitch`, `Harmony.scale`/`.chord` are
`ScaleField`/`Chord` — and constructing a sum type needs the **inject family**, which map #517 rules
out of scope until a consumer wants it. A `pack` that cannot be wired would be dead code and would
force the mechanism to emit a second, unusable operator per type. `unpack` is also the only half the
unbundling test needs. `pack` is recorded as a follow-up issue, to revive when leaf-promotion +
inject make a product type constructible.

### 2. Naming: the `pack`/`unpack` family

Adopt the ecosystem convention **`pack`/`unpack`** over the effort's working "make/break" language.
The research found `pack`/`unpack` universal and "make/break" used nowhere; matching the convention
lowers the learning curve for anyone arriving from Max/PD, and the latching those environments'
field outputs carry is documented behavior, not something the verb must encode. `split`/`join` is
rejected — it reads as stream routing, not struct field access. We build **`unpack`** now; the
deferred construct direction will be **`pack`**.

### 3. Naming surface

- **`type_name` = `unpack_<type>`**, verb-first snake_case — `unpack_note` — exactly parallel to
  `number_op`'s `add_f32_value`. Struct ident `UnpackNote`. The deferred direction → `pack_note`.
- **Output ports are the field names, verbatim** — `unpack_note` exposes `pitch` and `velocity`.
  The whole point is to address a struct's fields by their real names on the wire; any other naming
  is needless indirection.
- **The input port is `in`**, carrying the whole `Note` event (matching the `in`/`x` single-input
  convention of `map`/`abs`).

A mono voice then reads: `unpack_note` `in`←Note stream, `.pitch`→`resolve`, `.velocity`→envelope.

### 4. Mechanism: a one-line census macro in the operators layer

A **function-like macro** `unpack_op!(vocab::Note);`, invoked once per product type in a single
census file `operators/unpack.rs`. It reuses the **shared contract-rendering internals** that
`operator_contract!` / `number_operator_contract!` already use (the `render_contract` / `Port` /
`Descriptor` builders and the `naming` helpers), so a generated `unpack_note` is identical in shape
to a hand-written operator — same typed `IN_*`/`OUT_*` handles, same `Descriptor` — and
**self-registers through `inventory`** (`register_operator!`), with **no central match to edit**,
exactly as `number_op` mints `add_f32_value`.

Rejected: a **`#[derive(Unpack)]` on the vocab struct**. It is marginally more automatic (the struct
definition is the sole trigger), but it lands the generated operator's `process`/`Descriptor`/
`Operator` impl inside the **vocab module** — pushing operator-layer *behavior* into the pure-data
layer, a layering inversion. The census macro keeps vocab types free of the operator machinery, puts
generated operators where every other operator lives, and makes the set of unpackable types explicit
and auditable in one greppable file. "One census line" ≈ "one derive tag" in per-type effort, so the
extensibility goal is met either way; clean layering breaks the tie.

The set of unpackable types is thus **product vocab types only**. Sum-typed *fields* ride out as
opaque leaves (a `Pitch` output is a whole `Pitch`, per ADR-0062) — `unpack` never decomposes a sum
type; that is the deferred match family.

### 5. Latching semantics

`unpack_<type>` reads an **Event** stream on `in` and emits **held `Value`** outputs — each field
holds its last value until the next event (the Event→Value latch, ZOH, reuben's native `Value`
behavior). Two details:

- **Initial value (load-time / before the first event, or `in` unwired) = `<T as Default>::default()`.**
  The generator latches each field to the type's `Default`, staying fully type-agnostic. `Default`
  becomes a **requirement on any unpackable product type** — the idiomatic Rust expression of "the
  value before anything is set." Concretely: add `#[derive(Default)]` to `Pitch` with
  `#[default] Degree(0)` (tonic — stays in key) and to `Note`, giving a `{ pitch: Degree(0),
  velocity: 0.0 }` baseline. Because **velocity 0 is a note-off** (`Note::is_off`), nothing sounds
  at load: a downstream envelope's gate stays closed until the first real note overwrites the latch,
  so the tonic pitch baseline is musically a don't-care. Quiet-until-played, for free.
- **Simultaneous events: last-processed-wins.** Multiple notes at the same frame resolve by
  inheriting the Voicer's exact mechanism — snapshot the event stream sorted by frame, mutate the
  latched state in order; the last event at a frame sets the held value.

## Consequences

- **A mono voice becomes a patch.** `unpack_note` + `resolve` (#523) + osc/env expresses what the
  Voicer did monolithically, validating the mono-voice unbundling test (#518). The
  velocity-0-default latch means the Event→Value crossing yields mono note-priority *and* a
  correct quiet-at-load gate with no extra node.
- **Every product vocab type gains its `unpack` for one census line** — no central match, no
  per-type operator hand-authoring; `inventory` discovers the generated op, and the shared contract
  internals keep it indistinguishable from a hand-written operator. Adding a product type to the
  wire's decompose surface is a one-line edit in `operators/unpack.rs`.
- **`Default` is now load-bearing on unpackable vocab types** — a small, idiomatic contract; the
  first two impls (`Pitch`, `Note`) are `#[derive(Default)]` with a single `#[default]` attribute.
- **Hot path unchanged.** The generated `process` is a frame-sorted latch over `Copy` fields —
  allocation-free, the same shape as the Voicer's per-voice change loop.
- **Depends on ADR-0062 landing first** — `unpack_note`'s `pitch` output has no wire form until
  `Pitch` is leaf-promoted. Both are design-locked, not-yet-implemented; implementation order is
  leaf-promotion → unpack.
- **Deferred, recorded:** the **`pack`** (construct) direction — blocked on the inject family and
  wanted by no consumer yet — tracked as a follow-up issue; and sum-type field decomposition (the
  match family), already out of scope on map #517.
