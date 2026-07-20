# Why: A vocab enum that carries a payload is promoted to its own named `Arg` variant as an opaque `Copy` leaf, while an all-unit enum type-erases to `Arg::Enum(index)` — `#[derive(ArgValue)]` routes each enum by whether any variant carries a payload.

[Rule](../../composition-operators.md#payload-enum-arg-leaves)

The [Message/Arg substrate](message-arg-substrate.md) sorts every shared vocab type into one of two
homes on the central `Arg`: a **struct** (`Note`, `Harmony`) gets its own named variant because it
carries a real per-type shape with nothing to erase, while a **unit enum** (`FilterMode`, `GateMode`)
erases to the single `Arg::Enum(index)`, its identity moved into the port descriptor's `EnumMeta` so
adding one touches no central engine file. A **payload-carrying enum falls between the two.** `Pitch`
(`Degree(i32) | Absolute(f32)`) cannot take the index path — erasing it to a bare index would drop the
`i32`/`f32` payload — so before this rule it had no wire form at all, existing only *nested* inside
`Arg::Note`. That is the capability-trapped shape that blocks a mono voice from being a patch: you
cannot wire `unpack_note → pitch → pitch2freq` if the `Pitch` in the middle cannot ride a wire.

The resolution is the substrate's own logic taken to its conclusion: a payload enum carries a real
per-type shape (which case, plus its payload), *not* an index — so like a struct, **there is nothing
to erase**, and it is promoted to its own named `Arg` variant (`Arg::Pitch`), an opaque `Copy` leaf.
The rule is **generic, not `Pitch`-special**: `#[derive(ArgValue)]` routes *any* enum with a non-unit
variant to the struct glue, and keeps the all-unit erased path for the rest — one macro path serves
every future payload enum. Whole-enum in, whole-enum out; decomposing the internal `Degree`/`Absolute`
case is a separate concern (the deferred match/inject family, not this rule).

Two properties keep the promotion cheap and honest. It stays **one-line-plus-derive**: per type, a
`#[derive(ArgValue)]` plus two compiler-demanded central edits — the `Arg` declaration and one arm in
the sole exhaustive outbound match (`osc_out_args`). A type-erasing byte carrier (`Arg::Vocab{tag,
[u8;N]}`) was rejected for this tier: it buys "zero central edit" with `unsafe`/`bytemuck` transmute,
loses `Arg`'s `Debug` legibility and the per-type OSC checklist, and mixes a named and erased model —
machinery that outweighs appending one enum line. And a promoted type is **wire-internal by default**:
its `osc_out_args` arm is empty (like `Harmony`) unless a consumer needs an external OSC form, so the
exhaustive match is a feature — it forces every newly promoted type to consciously answer "does this
cross the OSC boundary?" The hot path is untouched (`Pitch` is 8 bytes and `Copy`; `Arg` stays bounded
by `Harmony`, allocation-free), and the operator API is unchanged — a handle still reads a real Rust
`Pitch`; the wire form is a storage detail below that surface.

Distilled from: ADR-0062
