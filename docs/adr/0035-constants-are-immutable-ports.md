# ADR-0035: Constants are immutable ports; the param concept is deleted

## Status

Accepted. Implemented — `Constant` ports live in `descriptor.rs`, the `param` concept is deleted,
and [ADR-0037](0037-typed-port-handles.md) builds on this.

Supersedes the param-coupling of [ADR-0028](0028-one-input-shape.md) (Constant declared *as a param*) and
completes the migration begun in [ADR-0030](0030-osc-as-all-data-one-message-type.md) /
[ADR-0031](0031-float-resolves-to-value-or-signal-by-wiring.md).

## Context

A node has exactly two kinds of surface:

- **Inputs** — *runtime*. A per-block/per-sample value the engine can change while the patch runs
  (wired from another node, or materialized/latched from a default).
- **Constants** — *plan-time*. A value fixed when the graph is instantiated; changing it rebuilds the
  graph (e.g. the voicer's `voices` pool size).

This was an explicit early decision. It kept getting violated by a third concept — **param** — a runtime
`f32` control that predated [ADR-0030](0030-osc-as-all-data-one-message-type.md). ADR-0030/0031 migrated
every *runtime* param to a materialized input. What remains is vestigial:

- `Descriptor.params: Vec<ParamMeta>` is non-empty for **exactly one** param across the whole operator
  set — the voicer's `voices` (`crates/reuben-core/src/operators/voicer.rs:46`) — and that one is a
  **Constant** (`constant: voices`), i.e. plan-time, not runtime. The `param:` contract keyword is used by
  zero operators.
- So the entire param machinery exists today to host one plan-time value that should never have been a
  param. It conflates the two honest surfaces.

Two further over-specializations sit on the same axis, in the *cold authoring layer*:

- `Node.input_overrides: Vec<(usize, f32)>` and `Node.enum_overrides: Vec<(usize, usize)>` — author
  value-overrides split **by type** (f32-as-`f32` vs enum-as-variant-index), written by two methods
  (`set_input`/`set_enum`) and consumed by two parallel arms in `seed_latch`. `Str`/`I32`/`Harmony` inputs
  have **no** override channel at all.

The runtime is **already generic**: every message-rate value — f32, a concrete enum variant, `Harmony`,
`i32`, `str` — is held as one [`Arg`](../../crates/reuben-core/src/message.rs) in
`PlanNode.latch: Vec<Arg>`. The per-type lanes were already collapsed by ADR-0030. Only the
storage/set/seed layer is still bifurcated, and it funnels into that single `Arg` regardless.

## Decision

1. **Delete the param concept.** Remove `Descriptor.params`, `Descriptor.constant_param`, `Node.params`,
   `ParamMeta`-as-param, `default_params`, `param_index`, `is_constant_param`, `constant_param`, and
   `set_param`. A node's descriptor surface is `inputs`, `outputs`, `constants`, `resources`.

2. **A Constant is an immutable port.** Add `Descriptor.constants: Vec<Port>`, reusing the `Port` struct
   wholesale (it already bundles name + type + meta). A constant differs from an input only in that it is
   plan-time/immutable; membership in the `constants` list (vs `inputs`) *is* that distinction. Constants
   stay out of every loop that walks `inputs` (edges, buffers, materialization), so they never acquire a
   wire or a per-sample buffer.

3. **Values are generic `Arg`.** Collapse `input_overrides` + `enum_overrides` into one
   `Node.value_overrides: Vec<(usize, Arg)>` (inputs), and add the symmetric
   `Node.constant_overrides: Vec<(usize, Arg)>` (constants). Both are **sparse** with descriptor-default
   fallback — a constant has its default in the descriptor meta too, so it is only stored when the author
   overrides it. Net on `Node`: three type-split Vecs → two Vecs split by the runtime/plan-time axis.

4. **One coercion seam.** A `Port::coerce(raw_literal) -> Result<Arg>` owns the only type-switch:
   f32 → clamp → `Arg::F32`, enum → `resolve_arg`, i32 → range-check → `Arg::I32`, etc. A single generic
   `set_value(node, name, raw)` (replacing `set_param`/`set_input`/`set_enum`) finds the port, calls
   `coerce`, and upserts `(idx, Arg)`. `seed_latch` collapses to one arm: `Arg` straight into `latch[port]`.
   `Str`/`I32`/`Harmony` inputs become settable for free.

5. **Rename `ParamMeta` → `F32Meta`.** It is the meta for a bounded f32 control (`name/min/max/default/
   unit/curve`), parallel to `EnumMeta`. No new vocabulary.

6. **`voices` becomes a true integer.** Carrier is the existing `Arg::I32` (OSC-native; no unsigned/usize —
   `usize` is platform-width and not wire-serializable, and the `1..=32` range enforced by `coerce` is the
   real guard against insane values, not the integer width). Add a small `I32Meta { name, min: i32,
   max: i32, default: i32 }` (no `unit`/`curve` — a count has no response curve), the third per-type meta
   alongside `F32Meta`/`EnumMeta`. The single pool-build site does `as usize`.

7. **Contract macro.** Replace the `params:` block and the `constant:` back-pointer with one `constants:`
   block that reuses the input port grammar:

   ```rust
   // before
   params:   { voices: { 1.0..=32.0, default 8.0, "", lin } },
   constant: voices,
   // after
   constants: { voices: i32 { 1..=32, default 8 } },
   ```

8. **On-disk format is unchanged.** Constants serialize to the patch's `config` block and input overrides
   to `inputs` — exactly as today. The serialized shape never depended on the internal param/override split.
   No migration, no version bump; existing instruments load and round-trip byte-identically. The
   `config` (plan-time) vs `inputs` (runtime) split on disk was already the honest model; this change just
   makes the internal code match it.

## Consequences

- Nodes have only inputs, outputs, constants, resources — the runtime/plan-time boundary is now structural
  and hard to re-violate.
- `Arg` is the one value carrier from authoring through runtime; the cold layer no longer bifurcates by type.
- One new settable type family (`I32`) lands generically — future counts (taps, steps) reuse it.
- The macro surface shrinks (one block, no back-pointer); `voices` reads honestly as `i32`.

---

# Implementation plan

Two sequenced steps, each independently reviewable, both guarded by the golden serialization tests
(`crates/reuben-core/tests/descriptor_golden.rs`, `crates/reuben-core/tests/wire_forms.rs`) proving the
on-disk format never moves.

## Step 0 — ADR (this document)

Land this ADR. Note the supersession of ADR-0028's param-coupling at the top of ADR-0028.

## Step 1 — Collapse author overrides onto generic `Arg` (no param concept touched)

Goal: retire the f32/enum override split. Independently valuable; leaves `params` in place.

1. **`Port::coerce(raw) -> Result<Arg>`** — new method on `Port` (`descriptor.rs`). Switch on `PortType`:
   `F32`/`F32Buffer{meta}` → clamp to `F32Meta` range → `Arg::F32`; `Vocab{enum_meta}` → `resolve_arg`;
   `I32` → range-check → `Arg::I32`; `Str`/`Harmony` → validate → `Arg`. This is the *only* type-switch.
2. **`Node`** (`graph.rs`): replace `input_overrides` + `enum_overrides` with
   `value_overrides: Vec<(usize, Arg)>` (sparse).
3. **`Graph::set_value(node, name, raw)`** replaces `set_input` + `set_enum`: resolve input port by name,
   `port.coerce(raw)`, upsert `(idx, Arg)`. Keep `set_param` temporarily delegating here for inputs (it
   dies in Step 2).
4. **`seed_latch`** (`plan.rs`): collapse the two arms to one — read `value_overrides`, else descriptor
   default, into `latch[port]` as `Arg`.
5. **Loader** (`format.rs`): point the input-parsing path at `set_value` (build the raw literal, let
   `coerce` do the typing). Save path reads `value_overrides`.
6. **Tests:** `descriptor_golden` + `wire_forms` must pass unchanged (byte-identical). Add a test that a
   `Str`/`I32` input override now round-trips (previously impossible).

## Step 2 — Delete params; constants become immutable ports

Now the param concept has no runtime users; remove it and re-home `voices`.

1. **`F32Meta`**: rename `ParamMeta` → `F32Meta` (mechanical, repo-wide). Add `I32Meta { name, min, max,
   default }`.
2. **`Descriptor`** (`descriptor.rs`): delete `params`, `constant_param`, `default_params`, `param_index`,
   `constant_param()`, `is_constant_param`. Add `constants: Vec<Port>` and the lookups it needs
   (`constant_index(name)`, `constant(name)`).
3. **`Port`**: add an `i32` constructor + `PortType::I32`-with-`I32Meta` support so a constant port can be
   integer-typed. (Audit whether `PortType::I32` already carries meta; add an `I32Meta` slot if not.)
4. **`Node`** (`graph.rs`): delete `params`. Add `constant_overrides: Vec<(usize, Arg)>` (sparse, default
   fallback). `add_boxed` no longer seeds `default_params`.
5. **`set_param`**: delete. `set_value` is the only entry point; extend it to route constant names to
   `constant_overrides` (still via `coerce`).
6. **`Plan::instantiate`** (`plan.rs`): the pool-sizing read switches from `Node.params[i]` /
   `constant_param` to `constants` + `constant_overrides`, decoding `Arg::I32(..) as usize`.
7. **Contract macro** (`reuben-macros/src/lib.rs`, `number_op.rs`): remove `params:`/`constant:` parsing;
   add the `constants:` block (reuse input port grammar, including the new `i32 { range, default n }` arm).
   Generate `constants: vec![..]`; drop `params`/`constant_param` from the generated `Descriptor`.
8. **voicer** (`voicer.rs`): rewrite the contract to `constants: { voices: i32 { 1..=32, default 8 } }`.
   Update the pool-build site to read the constant as `i32`/`as usize`.
9. **format.rs**: route `constants`/`constant_overrides` → `config` block (replacing the old
   `constant_param` routing). Output must stay byte-identical for `voices`.
10. **schema.rs / cli.rs (`describe`)**: iterate `constants` (+ `inputs`) instead of `params`; emit the
    integer range for `voices`.
11. **scaffold.rs**: update the operator scaffold template (it currently emits `constant: <name>`) to the
    `constants:` block; update its test (`scaffold.rs:476`).
12. **Hand-written descriptors / fixtures**: `output.rs` (`params: vec![]` → `constants: vec![]`),
    `tests/wire_forms.rs:43`, `tests/descriptor_golden.rs:112`, `tests/contract_port_types.rs:65`.

## Verification

- `descriptor_golden` + `wire_forms` green at every step (byte-identical serialization is the core claim).
- New: `voices` integer coercion + clamp test (`0` and `99` rejected/clamped to `1..=32`; `"8"` and `8`
  both parse to `Arg::I32(8)`).
- New: a `Str`/`I32` input override round-trips (the free channel from the generic collapse).
- `reuben describe voicer` shows `voices` as an integer constant in `config`, range `1..=32`.
- Full `cargo test` + load every instrument in the repo (granulator test instrument, etc.) and confirm
  unchanged render / round-trip.
- Run `/sync-docs` to bring ARCHITECTURE / authoring docs in line and regenerate the instrument schema.
