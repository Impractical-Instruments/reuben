# ADR-0037: Typed port handles + the buffer-presence invariant

## Status

Accepted (issue #164).

**Update (2026-07-10, issue #216):** the `pub(crate)` primitives this ADR demoted —
`Io::input`/`Io::output` and the `IoInput`/`IoOutput` traits — are since deleted outright; the
`form` impls absorbed them (each reads the private `Io` state directly, one dispatch per form), and
the "one sanctioned production use" (`osc_out`'s undeclared emit port) now writes through a local
inline `Out<Raw>` handle. No primitive layer remains beneath `io.read`/`io.write`.

Extends [ADR-0031](0031-float-resolves-to-value-or-signal-by-wiring.md) ("the form is the type" →
**"the form is the port"**) and amends [ADR-0030](0030-osc-as-all-data-one-message-type.md)'s
materialization (a new engine invariant). Builds on
[ADR-0025](0025-single-source-operator-contract.md)'s single-source contract; generalizes the
fully-generated deep path [ADR-0033](0033-number-operator-contract-macro.md) proved for the
stateless-pointwise family to every hand-written operator.

## Context

`operator_contract!` single-sourced the descriptor↔const-index binding (ADR-0025), but two seams
between the declaration and each hand-written `process` stayed open:

- **S1 — the type seam.** Nothing bound a port's declared `PortType` to the payload type `T` at
  `io.input::<T>(port)`. `io.input::<Note>(IN_FREQ)` compiled even though `freq` is `f32_buffer`,
  silently returning an empty `EventStream`; the finiteness-only driver test still passed.
- **S2 — default duplication.** The descriptor declared `default 440.0`; `process` restated it as
  `.unwrap_or(440.0)` — two sources that can drift. (In-engine the latch is always seeded from the
  descriptor, so the drifted literal is *misleading dead code* — worse than a live bug, because it
  reads as the truth.)

Each dense read also carried a defensive `.get(i).copied().unwrap_or(0.0)` guard against a short or
absent buffer — a guard the engine's materialization already made unnecessary, but nothing stated
that as an invariant, so every operator paid it.

Out of scope: the two-line `descriptor()`/`spawn()` delegates (S3). Rust's declarative macros
cannot inject methods into a separate `impl Operator`, so closing S3 needs a proc-macro attribute —
a separate, lower-value fight.

## Decision

Two reinforcing halves, landed atomically.

### Half 1 — the buffer-presence invariant (engine)

**Every declared `f32_buffer` input handed to `process` is a dense buffer of exactly `frames`
(sub-block) samples.** Materialization is total over Signal inputs:

- a meta-carrying signal control (scalar default / knob range) ZOH-fills from its latch default —
  as before (ADR-0030/0031);
- an unwired **bare** `f32_buffer` input fills with **silence** (zeros) — its latch seeds
  `Arg::F32(0.0)`, and `Plan::instantiate` gives every Signal input a buffer (wired share or
  materialized scratch), never `None`.

No operator ever sees `&[]` or a short slice, so `io.read(SIG)[i]` is safe by construction and the
per-read guards go. Pinned by `op_driver::unwired_bare_buffer_input_reads_length_n_zeros` and a
`debug_assert` inside the Signal read.

### Half 2 — typed handles + `io.read` / `io.write` (contract + Io)

`operator_contract!` emits a **typed const per port** whose *type* fixes the form and whose value
carries the declared default:

```rust
pub const IN_FREQ:     In<SignalF32>       = In::new(0, 440.0);
pub const IN_WAVEFORM: In<Held<Waveform>>  = In::new(1, Waveform::DEFAULT);
pub const OUT_AUDIO:   Out<SignalF32>      = Out::new(0);
```

Two verbs dispatch on the handle — `io.read(port)` / `io.write(port)` — lowering to the same latch
/ stream / buffer state as ADR-0031's primitives:

| declared form | marker | `io.read` returns | `io.write` returns |
|---|---|---|---|
| signal (`f32_buffer`, meta or bare) | `SignalF32` | `&[f32]`, length-`n` — index directly | `&mut [f32]` |
| held `f32` / `i32` | `Held<f32>` / `Held<i32>` | the scalar, defaulted to the declared default | `MsgWriter` |
| held enum | `Held<E>` | the enum, defaulted to `E::DEFAULT` (single-sourced with `EnumMeta.default`) | `MsgWriter` |
| held `Harmony` | `Held<Harmony>` | `Harmony`, defaulted to `Harmony::DEFAULT` (the new `const`, = `Default::default()`) | `MsgWriter` |
| `Note` event | `Event<Note>` | `EventStream<Note>` | `EventWriter` |
| `&Arg` pass-through | `Raw` | `EventStream<&Arg>` | `EventWriter` |

- **S1 shut**: the handle *is* the declared form. `io.read(IN_FREQ)` on a Signal handle cannot
  return an event stream; there is no `usize` const left to feed the primitives, so a wrong-form
  read does not compile from operator code.
- **S2 shut**: the held read's fallback is the default the handle carries — one datum, from the
  same contract tokens as the descriptor. `.unwrap_or(..)` disappears from held reads; the
  hand-written `defaults_are_data` style test has nothing left to guard for hand-written ops (the
  macro-generated one for number ops remains, comparing descriptor to declaration).
- **Default scope is held reads only.** Signal per-sample reads stay raw `&[f32]`: the old
  `.get(i).unwrap_or(0.0)` was a *defensive length* guard, not the musical default. Half 1 removes
  the need for the guard; the knob default must NOT fill "missing" samples — there are none. A
  Signal handle still *carries* its declared default, as data (`default_value()`), not applied at
  read.
- **Primitives demoted.** `Io::input` / `Io::output` (and the `IoInput`/`IoOutput` traits) drop to
  `pub(crate)` — the lowering seam and the hand-built-`Io` unit-test surface. The one sanctioned
  production use is the `osc_out` sink's write to its *undeclared* emit port (outbound taps drain
  by node, not by wired edge, so no handle exists for it).
- **`Constant`s get no handle.** `C_*` stays a bare `usize` ordinal: instantiate-time config is
  never read in `process` (ADR-0035).
- **Bare indices stay legal at the seams** via a `PortIndex` trait (`usize`, `In<F>`, `Out<F>`):
  `Graph::connect`/`tap_output`, `OpDriver::set/push/drive/output`, and `Io::varying` take either,
  so registry-driven harnesses and loaders keep working while operator tests address ports by
  handle.

### Naming (the ubiquitous language)

`CONTEXT.md` lists **"port"** as a term to avoid (`Input` is the domain word), so the handle types
are **`In` / `Out`** — not `InPort`/`OutPort` — and the consts keep their established
**`IN_*` / `OUT_*`** names (also load-bearing: an operator like `filter` has both an `audio` input
and an `audio` output, so prefix-less handle names would collide). "Handle" joins `CONTEXT.md` as a
deliberate term. Form markers live in `operator::form` (`SignalF32`, `Held<T>`, `Event<T>`, `Raw`),
mirroring the three wire forms of ADR-0031 plus the pass-through.

### Why big-bang

A `usize` const and an `In` const can't share a name, so dual emission was impossible: the macro
switch, the handle machinery, the engine invariant, and all ~30 operator bodies land as one atomic
change. Enforcement rests on the chain **macro-emission test → form marker → type system** (the
repo has no compile-fail harness, and the chain makes wrong-form reads uncompilable anyway).

## Consequences

- Operator bodies get shorter and honest: `io.read(FREQ)[i]`, `io.read(WAVEFORM)`,
  `io.write(AUDIO)` — no guards, no restated defaults, no turbofish. New-operator scaffolding
  (`reuben scaffold-operator`) emits the handle spelling.
- The descriptor, the const, the read shape, and the read default are one declaration; drift
  between them is a compile error or impossible, not a silent empty read.
- Dead dual-form reads were exposed and deleted during migration (e.g. `harmony.rs` scanned
  per-sample buffers its held Value inputs never had; `filter`'s const-fold path re-read its
  materialized buffer's latch through a second form — now `buf[0]` of the uniform buffer).
- Computed port indices must go through handle arrays (`sequencer::IN_STEPS`,
  `harmony::IN_STEPS`) instead of `IN_X + k` arithmetic — slightly more verbose, but the
  arithmetic was exactly the kind of untyped indexing this ADR exists to remove.
- The descriptor surface, JSON schema, wire format, and OSC boundary are unchanged — this is an
  authoring-surface change only; rendered output is bit-identical (the full behavioral suite,
  including the OpDriver sample-exact pin, passes unmodified).
- `Harmony` gains a `const DEFAULT` (and `ScaleField::MAJOR`), single-sourcing what
  `Default::default()` returns, so a held-`Harmony` handle can carry its default in a `const`.

## Test plan (as landed)

1. **Macro-emission** (token-string, `reuben-macros`): the emitted handle type + default per
   `PortType`, all forms covered.
2. **Handle machinery** (`operator::typed_handles`): each form's read/write shape; the held read
   applies the declared default; a Signal read is length-`n`; `varying` takes a handle or index.
3. **Engine invariant** (`op_driver`): an unwired bare `f32_buffer` input reads length-`n` zeros
   through the real engine seeding + stepping, including a partial final block.
