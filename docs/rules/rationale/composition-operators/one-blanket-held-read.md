# Why: Every vocab enum's held read comes from one blanket `InForm for Held<T>` impl, so the derive generates no per-type read glue.

[Rule](../../composition-operators.md#one-blanket-held-read)

`#[derive(ArgValue)]` generates what is genuinely per-type: the conversion into and out of `Arg`
(`to_index`/`from_index` for an all-unit enum, a named variant for [a payload-carrying
one](payload-enum-arg-leaves.md)). It deliberately generates **no** read glue — no `IoInput` impl,
no held-read helper — because reading a latched Value is not per-type behavior. It is the same
zero-order-hold in every case: take the latch's current `Arg`, convert it, hand it back as a
constant for this `process` call. Core states that once as a blanket impl over `Held<T>`, and every
enum in the vocabulary is covered the moment it can convert.

Generating one impl per enum looks free and is not: each
generated impl is an independent copy of the ZOH read, so changing how held values are read — a
different latch representation, a different miss policy, an added diagnostic — becomes an edit to
the macro plus a rebuild of every generated copy, and any enum whose derive predates the change
keeps the old behavior with no signal that it has diverged. One blanket impl means that change is
one edit and applies everywhere at once.

The absence is asserted rather than assumed. The derive's tests check that the expansion contains
*no* `IoInput` and no held-read helper, which is what keeps the rule from quietly eroding: adding
per-enum read glue would be an easy, plausible-looking change, and without a negative assertion it
would land silently and only be noticed the next time someone tried to change how held reads work.

Decided in: issue #639 — settled directly, no ADR.
