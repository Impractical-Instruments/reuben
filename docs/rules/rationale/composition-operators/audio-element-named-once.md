# Why: The audio element type and its buffer forms are named in exactly one site.

[Rule](../../composition-operators.md#audio-element-named-once)

`crates/reuben-core/src/signal.rs` is the single naming site: `AudioSample` (the element, `f32`
today), `Block` (one **owned** block of audio for a single edge, the pool entry the Plan allocates),
and `BlockView`/`BlockMut` (the **borrowed** read and write views an operator receives during
Render, so it touches audio without allocating). CV and audio are the same thing here — there is no
separate control-rate signal type; sub-audio-rate control travels as a `Message`.

The aliases are **zero-cost and transparent**: `AudioSample` *is* `f32`, `BlockView` *is* `&[f32]`.
Adopting them changes no layout, no ABI, and no runtime behavior, which is what makes the discipline
affordable — it buys naming, and costs nothing at the machine.

**The point is the boundary, not the abstraction.** The cheap justification for a type alias is
future-proofing: a change of element (`f32` → fixed-point, or `f64`) becomes one edit instead of a
repo-wide grep-and-replace. That is true but secondary, and on its own it would not be worth a rule —
the element has never changed. The real return is that forcing every `f32` buffer site to spell out
whether it is *permanently audio* (adopt an alias) or *incidental `f32`* (keep the primitive)
surfaces a partition the codebase otherwise leaves implicit. A reader meeting `&[f32]` in the render
spine cannot tell which one it is; a reader meeting `BlockView` can. That partition is real: the
device layer's interleaved frames, decoded resource bytes, and per-sample DSP arithmetic are all
genuinely *not* logical audio buffers, and each is allowlisted with its reason rather than converted.

Enforcement is a **text linter** (`scripts/check_sample_alias.py`), not clippy, for a mechanical
reason: a primitive slice of a primitive has no nameable type *path* to disallow — `[f32]` is not a
nominal type, so `disallowed-types` cannot express the rule. The guard therefore walks the tree for
raw `&[f32]`/`&mut [f32]`/`Vec<f32>` outside the naming site and the allowlist, in the same spirit as
the other repo linters. Fixed-size arrays (`[f32; N]`) and bare scalars are deliberately not flagged
— only the buffer forms the alias exists to name.

The allowlist is the load-bearing half and is kept tight: every entry names why the `f32` there is
not a logical audio buffer. An allowlist that grew by reflex would erase the partition the rule
exists to draw.

Decided in: issue #635 — settled directly, no ADR (the discipline and its guard predate the rule;
the sweep of `reuben-core` found the reasoning living only in the module doc and moved it here).
