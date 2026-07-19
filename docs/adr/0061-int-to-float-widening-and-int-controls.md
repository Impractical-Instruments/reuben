# ADR-0061: `i32→f32` widens implicitly; an `i32` interface pipe is an integer control

## Status

Accepted. Implemented — the widening lives in the load-time wire check (`format/mod.rs`), the
`i32` pipe in `pipe_descriptor`, and the runtime quantize in `render::held_arg`; euclidean-drums'
`steps`/`pulses`/`rotation` controls are the first users.

Builds on [ADR-0035](0035-constants-are-immutable-ports.md) (`I32` became a settable `Arg` carrier)
and [ADR-0031](0031-float-resolves-to-value-or-signal-by-wiring.md) (the local per-wire form check
and its one directional coercion). Amends [ADR-0038](0038-interface-pipes-and-the-device-layer.md)
(the pipe type set gains `"i32"`) and [ADR-0043](0043-surface-docs-decouple-presentation-from-instruments.md)
(the surface generator steps an integer control).

## Context

A control that is only ever a whole number — a Euclidean pattern's `steps`, `pulses`, `rotation` —
was declared `f32` and rounded at the point of use ([euclid](../../crates/reuben-core/src/operators/euclid.rs)
does `io.read(IN_STEPS).round()`). Two things were missing to let it be *typed* as an integer
instead:

1. **A control author had no way to say "this knob is an integer."** The instrument `interface`
   pipe types were `f32_buffer`/`f32`/`note`/`harmony`/enum — no `i32`. So a steps knob surfaced as
   a float control, and nothing downstream (a UI, a describe view, the value on the wire) knew it
   was integer-valued.
2. **Even if it could, the wire would be rejected.** euclid's ports stay `f32` on purpose — they sit
   in a modulation graph and must accept a continuous `f32` source (an LFO, an envelope) without a
   converter at every wire ([ADR-0031](0031-float-resolves-to-value-or-signal-by-wiring.md)'s
   argument for keeping modulatable controls `f32`). So an integer *control* feeding a float *port*
   is an `i32→f32` wire, and the load-time type check (`same_wire_type`, a discriminant compare)
   rejected it as a mismatch.

The machinery to make this work was almost entirely present. `Arg::as_f32` already maps
`I32(v) → v as f32`; the plan-time form check already classifies both `i32` and `f32` as **Value**,
so `i32→f32` already passes it via `(Value, Value)`; the contract macro already emits `Held<i32>`
for an `i32` port; `describe`/`introspect` already render `PortType::I32` (as kind `"int"`), and
`introspect` already calls the two numeric types "one wiring class". The only real gaps were the two
above.

Grilled in conversation: the question was whether the conversion should be an **explicit
`int_to_float` operator** or an **implicit coercion**. The type/mutability model was also
sharpened — `Input` (runtime) vs `Constant` (plan-time) is a mutability axis orthogonal to type;
the empty `(Int × runtime input)` cell was a practicality gap (redundant with float-round), not a
category law, and this ADR fills it.

## Decision

### `i32→f32` (and `i32→f32_buffer`) is an implicit, directional numeric widening

An `I32` source wires straight into an `F32` or `F32Buffer` sink. No operator, no ceremony. The
justification is that this is **not a shape crossing** — both are the numeric wiring class, both
(for `i32→f32`) the same **Value** form — and the coercion is **total and lossless** (every `i32` in
a control range is a distinct `f32`; the read already goes through `Arg::as_f32`). An explicit
converter node would be pure boilerplate on every integer-control patch.

It is **directional**, mirroring the one coercion [ADR-0031](0031-float-resolves-to-value-or-signal-by-wiring.md)
already sanctions:

| coercion | legal? | why |
|---|---|---|
| `Value → Signal` | implicit | lossless materialize (ZOH into a buffer) |
| `Signal → Value` | **hard error** | lossy (which sample?) — needs an explicit sampler |
| `i32 → f32` / `i32 → f32_buffer` | implicit (this ADR) | lossless widening; read via `as_f32` |
| `f32 → i32` | **hard error** | lossy (needs a rounding *decision*) — needs an explicit quantizer |

`f32→i32` stays rejected because it forces a rounding choice; the quantizer op that would bridge it
does not ship yet, exactly as the envelope-follower for `Signal→Value` does not. Rejected
alternative — **an explicit `int_to_float` operator**: more visible in the graph and consistent with
the "cross-*type* crossing needs an operator" phrasing, but that phrasing is about *shape/form*
crossings; a lossless widening within the numeric class earns no node.

Mechanically this is one arm added to the `compatible` gate in `format/mod.rs`'s pass-2 wire check —
a local per-wire decision, no propagation, in the spirit of ADR-0031's revision.

### An `i32` interface pipe is an integer control

`interface.inputs` entries may declare `"type": "i32"`. The pipe synthesizes an integer port
carrying its range + default in `PortType::I32`'s `I32Meta`, parallel to how an `f32` pipe carries
an `F32Meta`. Rules specific to an integer control:

- `default`/`min`/`max` must be **whole numbers** — a fractional literal is an authoring mistake,
  refused at load, *not* silently rounded (the round is for runtime traffic, below).
- **no `curve`** — a count has no response curve (`I32Meta` carries none).
- It **widens** (above) into a consumer's `f32` port, so an operator keeps its modulatable `f32`
  ports while the *control* is honestly integer.

### An `i32` value port quantizes runtime messages

`render::held_arg` — the seam every runtime Value message passes through — now rounds-then-clamps an
incoming value to `i32` for an `I32` port, exactly as `Port::coerce` already does for an authored
literal. So `/steps/in 12.7` latches `13`, not `12.7`: the "integer control" promise holds for live
OSC input, not only for authored defaults. (Before this, the `I32` arm cloned the arg uncoerced;
no runtime `i32` input existed, so the behavior was dead — this makes it correct for the new pipes.)

### The surface generator steps an integer control

The TouchOSC generator ([ADR-0043](0043-surface-docs-decouple-presentation-from-instruments.md))
treats an `i32` pipe as a fader (it is not skipped) and detents its grid onto the whole values in
`[min, max]`. This is presentation only — the engine quantizes regardless (the pipe rounds), so a
continuous drag still lands on an integer at the pipe; the detent just makes the knob *read* as
stepped. True hardware detenting is a target-by-target refinement.

## Consequences

- **euclidean-drums' `steps`/`pulses`/`rotation` are `i32` controls.** They widen into euclid's
  unchanged `f32` ports; `describe` reports them `kind: "int"`; the library index shows `i32`. The
  render is **bit-identical** to the pre-change `f32` version (euclid rounded anyway, and every
  driven gesture is integer-valued) — the `seed_recipes.rs` / `format_v3_rewrite.rs` bit-identity
  guards pass unchanged, so they now *validate* the widening rather than needing a re-bless.
- **euclid's operator ports stay `f32`.** The integer-ness lives in the *control*, not the port, so
  the ports still accept `f32` modulation sources. This is the deliberate split: the `f32` is the
  transport, the `i32` control is the meaning.
- **`f32→i32` remains unsupported** — a documented gap, not an oversight; it lands with an explicit
  quantizer op when something needs it. No operator has an `i32` *input* yet, so the reverse is
  currently unreachable through a real graph regardless.
- **The pipe type set is `f32_buffer`/`f32`/`i32`/`note`/`harmony`/enum.** Save (`pipe_type_name`,
  `pipe_doc_from_descriptor`) round-trips an `i32` pipe's type + range + default.
- **Docs swept:** ARCHITECTURE's per-wire-check summary, the authoring guide's coercion table and
  pipe-type list, and the control-surface generator all name the widening + integer control.
