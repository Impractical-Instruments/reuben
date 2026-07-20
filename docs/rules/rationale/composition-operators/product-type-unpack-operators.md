# Why: Each product vocab type gets a generated `unpack_<type>` operator from a one-line `unpack_op!` census entry that reuses the shared contract internals and self-registers through inventory, emitting every field as a ZOH-held Value defaulting to the type's `Default`.

[Rule](../../composition-operators.md#product-type-unpack-operators)

Capability was trapped inside monolithic operators, and the **Voicer** is the worst offender: it
welds note-priority bookkeeping, the Event→Value latch of held pitch/gate, harmony resolution, and
voice allocation into one opaque box. Expressing a mono voice as a patch (`unpack` Note → resolve →
osc/env) needs a wire operator that turns a `Note` **event** stream into its held fields — and nothing
in the registry did that. Everything that touches a `Note` keeps it whole (`sequencer`, `transpose`,
`snap`, `chord`); only the Voicer lowered it, internally. A `Note` field crossing from an Event stream
to a Value an oscillator can read is exactly the Event→Value boundary the wire-form check rejects
without an explicit latch ([per-wire-form-check](per-wire-form-check.md)), and no such latch node
existed.

The shape of the fix follows the guiding principle *low-effort-to-extend, no per-type hacks*: whatever
gives one product type its field operators must give **every** product type theirs at near-zero
marginal cost — the way [`number_operator_contract!`](pointwise-number-operators.md) mints a whole
family from one declaration and `inventory` discovers them with no central match. So a **one-line
census macro** `unpack_op!(vocab::Note);`, invoked once per product type in a single greppable census
file, reuses the shared contract-rendering internals (`render_contract`/`Port`/`Descriptor`, the
`naming` helpers) so a generated `unpack_note` is identical in shape to a hand-written operator — same
typed `IN_*`/`OUT_*` handles, same `Descriptor` — and self-registers through `inventory`, no central
match to edit. A `#[derive(Unpack)]` on the vocab struct was rejected: it lands the operator's
`process`/`Descriptor`/`Operator` impl inside the pure-data vocab module, a layering inversion pushing
operator behavior into the data layer; the census macro keeps vocab types free of operator machinery
and puts generated operators where every operator lives. Output **ports are the field names verbatim**
(`unpack_note` → `pitch`, `velocity`) — the whole point is to address a struct's fields by their real
names; the input port is `in`, matching the `in`/`x` single-input convention. Adopting the ecosystem's
universal **`pack`/`unpack`** verb pair (Max/PD) over the effort's working "make/break" lowers the
learning curve; `split`/`join` reads as stream routing, not field access.

Two semantic choices are load-bearing. Each field emits a **ZOH-held Value** (the native Event→Value
latch), and the initial value — before the first event, or with `in` unwired — is `<T as
Default>::default()`, which keeps the generator fully type-agnostic and makes **`Default` a requirement
on any unpackable product type**, the idiomatic Rust expression of "the value before anything is set."
`Pitch` defaults to `Degree(0)` (tonic, stays in key) and `Note` to `{ Degree(0), velocity: 0.0 }`;
because velocity 0 is a note-off, nothing sounds at load — a downstream envelope's gate stays closed
until the first real note, so the tonic baseline is musically a don't-care: quiet-until-played, for
free. Simultaneous events at one frame resolve **last-processed-wins**, inheriting the Voicer's exact
mechanism (snapshot sorted by frame, mutate the latch in order). Scope is deliberately **`unpack` only**:
the construct direction `pack` is deferred because every product type has a sum-typed field that cannot
be constructed on the wire without the (out-of-scope) inject family, so a `pack` would be dead code;
`unpack` is also the only half the unbundling test needs. The set of unpackable types is thus product
vocab types only — a sum-typed field rides out as an opaque leaf (a whole `Pitch`, per
[payload-enum-arg-leaves](payload-enum-arg-leaves.md)); `unpack` never decomposes a sum type.

Distilled from: ADR-0063
