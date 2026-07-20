# ADR-0062: Payload-carrying vocab enums are first-class `Arg` leaves (leaf-promotion)

## Status

Accepted (2026-07-20). **Implemented** (2026-07-20,
[#536](https://github.com/Impractical-Instruments/reuben/pull/536)) — the `ArgValue` derive routes
any payload-carrying enum to its own named `Arg` variant (`Arg::Pitch`); design was proven first by a
reference prototype on branch `prototype/leaf-promotion-pitch`. Decided through wayfinder map
[#517](https://github.com/Impractical-Instruments/reuben/issues/517), ticket
[#519](https://github.com/Impractical-Instruments/reuben/issues/519).

Amends [ADR-0030](0030-osc-as-all-data.md) — extends its 2026-06-30 amendment (the
struct-vs-enum split of vocab types across the central `Arg`) to the one case that amendment left
in a gap: enums that carry a payload. Foundational for the sibling tickets **make/break**
([#522](https://github.com/Impractical-Instruments/reuben/issues/522)) and **resolve**
([#523](https://github.com/Impractical-Instruments/reuben/issues/523)), which cannot decompose a
`Note` or resolve a `Pitch` until a `Pitch` can ride a wire on its own.

## Context

ADR-0030's amendment sorted every shared vocab type into one of two homes on the central `Arg`:

- **Structs** (`Note`, `Harmony`) → their **own named variant** (`Arg::Note`, `Arg::Harmony`),
  "because they carry a real per-type shape (a `Note` is pitch + velocity, not an index), so there
  is nothing to erase."
- **Unit enums** (`FilterMode`, `GateMode`, …) → the single type-erased `Arg::Enum(u32)` index,
  identity moved into the port descriptor's `EnumMeta`, so adding one touches no central engine
  file.

**Payload-carrying enums fell between the two.** `Pitch` (`Degree(i32) | Absolute(f32)`) is the
only one in the tree today, and `#[derive(ArgValue)]` **hard-rejected** it (`argvalue.rs`: "enum
variants must be unit variants"). The index path is unavailable to it — erasing `Pitch` to a bare
index would **drop the `i32`/`f32` payload**. So `Pitch` could not ride the wire alone; it existed
only *nested* inside `Arg::Note`. That is exactly the capability-trapped-inside-a-monolith shape
this map set out to dissolve: a mono voice cannot be `break(Note) → resolve → osc/env` if the
`Pitch` between `break` and `resolve` has no wire form.

## Decision

**A payload-carrying enum is promoted to its own named `Arg` variant — an opaque `Copy` leaf,
treated exactly like a struct.** The reasoning is ADR-0030's own: a `Pitch` carries a real per-type
shape (which case, plus its payload), *not* an index, so — like a struct — there is nothing to
erase. Whole-enum in, whole-enum out; the internal `Degree`/`Absolute` case is invisible to the
wire (decomposing it into its case is a *separate* future concern, the deferred match/inject
family — [#517](https://github.com/Impractical-Instruments/reuben/issues/517) Out of scope).

The rule is **generic**, not `Pitch`-special: `#[derive(ArgValue)]` routes **any** enum with a
non-unit variant to the struct glue (`From`/`TryFrom`/`FromArg`, own named variant); an **all-unit**
enum keeps the `Arg::Enum(index)` path. One macro path serves every future payload enum.

**Promotion stays one-line-plus-derive; no registration indirection.** Per promoted type the cost
is a `#[derive(ArgValue)]` on the type plus two compiler-guided central edits: the `Arg` enum
declaration (`message.rs`) and one arm in the outbound `osc_out_args` match (`boundary.rs`, the sole
exhaustive match on `Arg` — every other match falls through a wildcard and needs no edit). A
type-erasing carrier (an `Arg::Vocab{tag, [u8;N]}` byte buffer) was considered and rejected for this
tier: it buys "zero central edit" with `unsafe`/`bytemuck` transmute, the loss of `Arg`'s `Debug`
legibility, a mixed named/erased model, and the loss of the per-type OSC checklist below — machinery
that outweighs appending one enum line.

**Wire-internal by default.** A promoted type gets **no** external OSC form unless a consumer needs
one — its `osc_out_args` arm is empty, exactly like `Harmony`. `Pitch` is wire-internal: no
controller sends a bare pitch (`Note` already crosses as `/note pitch vel`), so it has no `OscArg`
/`register_osc_form!`. The exhaustive `osc_out_args` arm is therefore a *feature*: the compiler
forces every newly promoted type to consciously answer "does this cross the OSC boundary?"

### Extensibility tiers (why this tier, and what it defers)

The long-term goal is **user extensibility** — an external author adds a wire type (crossing the
OSC boundary) with no `reuben-core` change and no recompile. That is a distinct, larger effort, and
naming the tiers shows why this ADR does not reach for it:

1. **Tier 1 — this ADR.** Builtin types, per-type central edit, recompile.
2. **Tier 2 — byte carrier.** Builtin types, derive-only, no central edit — but *still a
   recompile*; the type must be compiled into the binary.
3. **Tier 3 — true user extensibility.** External types, no core change, no recompile. Needs a
   runtime type registry + dynamic carrier + descriptor/OSC-form registration at load time; and
   because a wire type like `Pitch` carries *behavior* (it resolves through `Harmony` to Hz — see
   [#523](https://github.com/Impractical-Instruments/reuben/issues/523)), a behavior-carrying user
   type implies plugin/wasm loading.

Even Tier 2 does not deliver Tier 3, so the Tier 1-vs-Tier 2 choice was never the extensibility
endgame. Tier 3 is **out of scope** for this map (a future effort). Tier 1 does **not foreclose**
it: a generic `Arg::Extern{tag, bytes}` carrier can later sit *alongside* the named builtin
variants — builtins and user types coexist.

## Consequences

- **`Pitch` becomes wire-expressible**, unblocking `break(Note) → Pitch → resolve → freq` — the
  mono-voice unbundling test ([#518](https://github.com/Impractical-Instruments/reuben/issues/518)).
- **Hot path unchanged.** `Pitch` is 8 bytes and `Copy`; `Arg` stays bounded by `Harmony`
  (`arg_stays_small` holds) and allocation-free. A named-variant read is a direct match, no index
  decode.
- **Adding a payload enum is local + compiler-guided.** Derive + two arms the compiler demands;
  no downstream match churn. Promoting a *unit* enum remains zero central edits (the `Arg::Enum`
  path). Promoting a struct is unchanged.
- The **operator API is untouched** — a handle still reads a real Rust `Pitch`; the wire form is a
  storage detail below that surface, consistent with ADR-0030.
- **Deferred, recorded:** the match/inject family (decomposing a `Pitch` into its case) and Tier-3
  user extensibility — both future efforts, neither needed by the first consumer.
