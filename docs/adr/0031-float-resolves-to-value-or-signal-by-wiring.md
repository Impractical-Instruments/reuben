# `Float` resolves to a Value or a Signal by wiring; buffers exist only when needed

## Status

Accepted (2026-06-27). Resolved in a grilling session. Supersedes the **"`Float` is always a
buffer underneath"** / static-read-view / always-materialize decisions of
[ADR-0028](0028-one-input-shape.md) and [ADR-0030](0030-osc-as-all-data-one-message-type.md), and
the **"all numeric operands are materialized `Float`"** rule of
[ADR-0029](0029-math-family-dense-float-one-file-per-op.md). Builds on — does **not** retract —
0030's foundation: one `Message = { address, frame, Arg }`, one closed `Arg`, one per-port ZOH
latch, `Signal` = a Message whose `Arg` is a `Buffer`.

## Context

[ADR-0030](0030-osc-as-all-data-one-message-type.md) collapsed seven carriers into one Message
stream read three ways. It kept one performance shortcut from
[ADR-0028](0028-one-input-shape.md): a `Float` is **always materialized into a per-sample buffer**,
and an operator picks a **static** read view (`io.signal` for per-sample DSP, `io.last`/`io.value`
for block-rate) fixed at authoring, *never* conditional on what is wired. Const-folding via a
`varying` hint was offered as an optional optimization on top.

**That "always materialize" was a mistake that slipped through, not a considered decision** — it
was carried forward from 0028's draft without anyone weighing the allocate-and-fill cost against
keeping a `Float` held. This ADR corrects it rather than overturns a deliberate trade-off.

That shortcut is the problem this ADR fixes. A `Float` that changes rarely — a `cutoff` knob, a
`tempo`, a gate that fires twice a second — still pays a **per-sample price it does not owe**:

- the engine **allocates and fills a `frames`-length buffer every block** for a value that is
  constant across it, and
- any operator reading it per-sample does **48k iterations/second** of work whose answer changes,
  say, twice.

The `varying` hint patches the *recompute* half (a filter can skip recomputing coefficients) but
not the *allocate-and-fill* half: the buffer is materialized regardless. The two read views
(`signal` vs `last`) being a static author choice means a `Filter` **always** materializes
`cutoff`, even when fed a literal.

The deeper miss: `Float` is the only `Arg` type for which "dense vs held" is left undecided and
then resolved by *always picking dense*. Every other type already lives in its natural form —
`Enum`/`Harmony` are held (latch, block-sliced), `Note` is a sparse event stream. `Float` should
too: **held by default (a Value), dense (a Signal) only when the graph actually needs per-sample
resolution.** And whether it needs that is knowable at plan time from the wiring.

This is pre-alpha; breaking changes are acceptable. The code is mid-migration toward 0030 —
`plan.rs` still carries `PortKind { Dense, Held, Stream }` with `port_kind()` mapping
`F32 => Dense` (`crates/reuben-core/src/plan.rs`), and `Io` still exposes
`signal`/`last`/`stream`/`varying`/`signal_mut`/`emit` (`crates/reuben-core/src/operator.rs`). This
ADR settles the `Float` story so the migration finishes against the right target.

## Decision

### Three runtime forms, derived from two axes

A wire carries one of **three forms**, replacing `PortKind { Dense, Held, Stream }`:

| Form | latched? | valued | domain | examples |
|---|---|---|---|---|
| **Value** | yes | single | sparse (Message-rate) | `f32`, `Enum`, `Harmony`, `i32`, `Str` |
| **Event** | no | multi | sparse (Message-rate) | `Note`, a note generator's output |
| **Signal** | — | per-sample | dense (audio-rate) | oscillator/filter audio, an LFO |

`PortKind` becomes **`{ Signal, Value, Event }`**. The two sparse forms fall out of two
independent axes — *latched?* and *single-valued?* — whose only sensible combinations are
**Value** (latched ∧ single) and **Event** (unlatched ∧ multi). The other two combinations are
nonsense (a latched-but-multi value has no single current state; an unlatched single value has
nothing to persist), so the set is closed at three.

**Slicing is derived, not declared.** Block-slicing exists for exactly one purpose: make a
piecewise-constant **latched single value** read as a constant within a `process()` call. So
**slice = latched ∧ single-valued = Value.** Events are not sliced (the consumer reads
frame-stamped events directly and may see several at one frame — a chord — which has no single
slice value); Signals are not sliced (dense by definition). The engine derives slice-or-not from
the value type `T`, not from a hand-set flag.

### `Float` resolves to Value or Signal; only `f32` can be either

A Value widens to a Signal **iff a buffer type exists for it**, and **`f32_buffer` is the only
buffer type** (audio is f32-only). Therefore `f32` is the *only* type that can take either form;
`Enum`/`Harmony`/`i32`/`Str` are always Value (no buffer type), `Note` is always Event. The form a
`Float` takes is **not authored** — it is resolved per node at plan time (below).

The contract no longer needs `float`/`buffer`/`control`/`signal` keywords. A port is declared by
its **value type**, and the type alone says which forms are legal:

| Declared type | Forms it can take | Default | Used for |
|---|---|---|---|
| **`f32`** | Value, or widened to Signal when wired to a Signal source | **Value** (no buffer, compute-on-change) | knobs / CV that may or may not be modulated — `cutoff`, `freq`, `gain` |
| **`f32_buffer`** | Signal only (a Value source is widened in) | Signal | true per-sample sinks/sources — `Filter.audio`, oscillator out |
| **`enum(T)` / `harmony`** | Value only | Value | `mode`, `waveform`, tonal context |
| **`note`** | Event only | Event | `Note` ports |

So `f32` vs `f32_buffer` *is* the value-capable-vs-force-Signal distinction — expressed in the
project's own terminology, no extra keyword. The `buffer` keyword and `PortType::Buffer` are
renamed **`f32_buffer`** throughout, leaving room for future `Signal<T>` buffer kinds without the
name lying.

### Outputs carry a denseness tag; propagation resolves the rest

An output declares one of four dispositions (the `f32` cases are the new part):

- **`f32_buffer` out** → **always Signal** (oscillator: generates per-sample regardless of inputs).
- **`f32` out** → **propagating**: resolves to **Value if every `f32` input resolved Value, else
  Signal**. The default for math/shaper ops (`add`, `mul`, `map`, `power`). Only `f32` inputs gate
  this; `Enum`/`Harmony`/`i32` inputs are always Value and never force the output dense.
- **`enum(T)` / `harmony` out** → always Value (e.g. `euclid`'s gate: a level that changes high at
  frame F, low at F+1 — two changes per fired step, *not* 48k samples).
- **`note` out** → always Event (note generators).

**Plan-time resolution is a topological forward pass** at instantiate:

1. **Seed** source outputs from their declared disposition; **unwired `f32` inputs resolve to a
   Value** holding the param default (no buffer — this replaces 0028/0029's "materialize the
   default into a buffer").
2. In topo order, each node takes its `f32` inputs' forms from their wired sources (an `f32_buffer`
   input forces Signal, widening a Value source); computes its outputs per the tags above; feeds
   downstream.
3. **Allocate an `f32_buffer` only for Signal-resolved ports.** Value ports get a latch slot only.
   Block-slice a node at the union of its Value inputs' change frames.

**Feedback cycles** break topological order. Detect the cycle, **pin the chosen back-edge to
Signal**, resolve forward from there, and log it. Audio feedback is per-sample in practice, and
Signal is the safe over-approximation: worst case you materialize a buffer that might have stayed a
Value — correctness preserved, only the perf win lost on that one edge. `i32`/`Str` have no buffer
type and never propagate to Signal (treated like `Enum`).

### Read / write API: two generic verbs, a sum type for `f32`

The split `io.signal`/`io.last`/`io.stream`/`io.varying` (read) and `io.signal_mut`/`io.emit`
(write) collapse into **`io.in::<T>(port)`** and **`io.out::<T>(port)`**, keyed on `T` via an
associated-type trait (`trait PortValue { type In; type Out; }`). The return *shape* is a sum type
only for the form-capable type (`f32`); message-only types return their value/iterator directly:

```rust
// f32 — the only two-form type:
match io.in::<f32>(IN_CUTOFF) {
    F32In::Value(c)   => { let co = coeffs(c, ...); for i in 0..n { /* one loop, c held */ } }
    F32In::Signal(buf)=> { for i in 0..n { let c = buf[i]; /* per-sample */ } }
}
match io.out::<f32>(OUT) {
    F32Out::Value(w)    => w.set(frame, value),   // frame-addressed, deduped, last-wins
    F32Out::Signal(buf) => { for i in 0..n { buf[i] = ... } }
}

// message-only types — no sum, no varying:
let mode    = io.in::<FilterMode>(IN_MODE);   // plain Value
let harmony = io.in::<Harmony>(IN_HARMONY);   // plain Value
for ev in io.in::<Note>(IN_NOTES) { ... }     // Event iterator
```

- **`F32In::{ Value(f32), Signal(&[f32]) }`**, **`F32Out::{ Value(MsgWriter), Signal(&mut [f32]) }`**.
  The variant *is* the answer `varying()` used to give, so **`varying()` is deleted**. The held
  fast-path stops being an optional optimization and becomes the structure the compiler forces.
- **`MsgWriter::set(frame, value)`** is the single write primitive (lowers to today's
  `Emit → Event → latch`): **deduped** against the current latch (a no-op change emits nothing —
  the wire stays genuinely sparse) and **last-write-wins** per frame. `euclid` calls it twice:
  `set(f, 1.0)`, `set(f + 1, 0.0)`.
- **Direct accessors for always-one-form ops** skip the dead arm: `io.in_signal(port) -> &[f32]`,
  `io.in_value::<T>(port) -> T`, `io.out_signal(port) -> &mut [f32]`,
  `io.out_value::<T>(port) -> MsgWriter`. Only *propagating* ops write the `match`.
- The old verbs (`signal`/`last`/`stream`/`varying`/`signal_mut`/`emit`) are **deleted, no compat
  wrappers** — keeping them reintroduces the "which verb?" ambiguity this removes.

### Coercion: one implicit widening, everything else explicit

The **only** implicit conversion is **Value → Signal at a Signal sink** (ZOH-materialize the
latched value into the block buffer at its change frame — 0030's one bridge, now firing *only* at a
genuine `f32_buffer` boundary fed by a Value, never speculatively). Every other cross-form wiring
is a **hard error at plan time**, fixed by inserting a named converter operator:

- **Signal → Value** — illegal (lossy/ambiguous: which sample?); needs an explicit sample-and-hold.
- **Event ↔ Value** — illegal; needs an explicit latch/hold (Event→Value) or change-detect
  (Value→Event) op.
- **Event ↔ Signal** — illegal; no meaning without an explicit op.

### Addresses are a boundary concept

Internal wires route by connection (`src_port → dst_port`), so internal Value/Event writes are
**addressless** — `MsgWriter::set(frame, value)` carries no address, and the `address` field drops
from the internal `Emit`/hot path. This finishes what 0030 started ("address … never internal
dispatch"). Addresses survive only at OSC/MIDI **boundary operators**, which map address ↔ port.

## Considered alternatives

- **Keep 0030 as-is (always materialize, static read view, `varying` for const-fold).** Rejected:
  it leaves the allocate-and-fill cost on every held `Float` and the static read view forces it.
  The whole point is to *not build the buffer* when a Value suffices.
- **Author declares held-vs-dense per port (Model A).** Rejected: forces a false choice on every
  modulatable control — declare `cutoff` held and you can't sweep it with an LFO; declare it dense
  and a constant still pays. It would fork the operator library into "modulatable" and "cheap"
  variants.
- **A `control` capability keyword (Value-or-Signal).** Rejected as redundant: "can be either" is
  already implied by "is `f32`" (the only type with a buffer form). The capability is a property of
  the type, not a thing to declare.
- **Re-introducing the `Delivery × Data` two-axis model 0030 rejected.** This is *not* that. 0030
  rejected an **author-declared** second axis. Here the form is **engine-resolved by wiring**
  (propagation), and `Value`/`Event`/`Signal` are the *runtime manifestations* of one Message
  stream — 0030's "read styles" — now named because the engine must pick one per port to decide
  buffer allocation. The operator handling Value-vs-Signal at runtime supersedes 0030's *static
  read view*, which only held together because of the always-materialize slip it leaned on.
- **A single always-per-sample read view (`F32In` iterator that yields per-sample regardless).**
  Rejected: it throws away the held fast-path — back to 48k iterations.

## Consequences

- **Breaking, engine-wide.** `PortKind { Dense, Held, Stream } → { Signal, Value, Event }`;
  `port_kind()` and the materialize-always path in `plan.rs`/`render.rs` are rewritten as the
  topological resolution pass. `Io` exposes `in`/`out` (+ `in_signal`/`in_value`/`out_signal`/
  `out_value`) in place of `signal`/`last`/`stream`/`varying`/`signal_mut`/`emit`. `varying()` is
  gone. The golden descriptor snapshot and generated instrument schema are re-blessed.
- **Buffers are allocated only for Signal-resolved ports.** A graph of held `Float`s (a `tempo`
  feeding a clock, a constant `cutoff`) allocates **zero** signal buffers for those edges and does
  **zero** per-sample work on them.
- **Supersedes** 0028/0030's "`Float` is always a buffer underneath" and static read-view, and
  0029's "all numeric operands are materialized `Float`" — `add(const, const)` now stays a Value
  (no buffer), going dense only when an input is a Signal. 0029's scalar-fn + dense-shell structure
  survives; the shell is now selected by the resolved form (the `match` / direct accessor).
- **`f32_buffer` replaces `Buffer`** as the type/keyword name (`PortType`, `Arg::Buffer`,
  contract macro, schema). `float`/`buffer`/`control`/`signal` keywords retire from the contract;
  ports are declared by value type (`f32`, `f32_buffer`, `enum(T)`, `harmony`, `note`).
- **`MsgWriter` is deduped + frame-addressed**; the internal `Emit`/`Event` path drops `address`.
  Addresses live only in boundary operators.
- **Amends** [ADR-0011](0011-message-delivery-and-timing.md) (block-slicing now serves Value ports
  — derived as latched ∧ single-valued — not `Float`), and the materialize bridge of
  [ADR-0030](0030-osc-as-all-data-one-message-type.md) (fires only at an `f32_buffer` sink fed by a
  Value, never speculatively).
- **`map` stays event-domain** for now (its Float reframe is staged with the instrument migration,
  per [ADR-0029](0029-math-family-dense-float-one-file-per-op.md)); it is untouched here.
- **Authoring contract:** `docs/agents/authoring.md`, `ARCHITECTURE`, `CONTEXT.md`, and the
  create-operator skill are swept to teach: declare by value type; `f32` is a Value that the engine
  may widen; `match io.in::<f32>` / direct accessors; the four output dispositions; the one legal
  implicit coercion.

## Implementation plan (sequenced; each step compiles + tests green before the next)

A separate thread from the decision, sketched here so the migration is ordered, not discovered.

1. **Forms + resolution, behind the current API.** Introduce `PortKind { Signal, Value, Event }`
   and the topological resolution pass in `plan.rs` (incl. unwired-`f32` → Value-default, feedback
   back-edge → Signal, buffer allocation gated on Signal). Keep the old `Io` verbs working over the
   new resolution (a Value-resolved `f32` read via the old `io.signal` still materializes — no
   behavior change yet). Land + bless the descriptor snapshot for any contract-shape changes.
2. **`Arg::Buffer → Arg::f32_buffer`** rename + `PortType` rename + contract-macro keyword change
   (`buffer → f32_buffer`, drop `float` in favor of `f32`). Mechanical, repo-wide; re-bless schema.
3. **New `Io` API.** Add `io.in`/`io.out` + the four direct accessors + `F32In`/`F32Out` +
   `MsgWriter` (deduped, frame-addressed, addressless); add the `PortValue` associated-type trait.
   Keep old verbs temporarily.
4. **Output denseness tags** in the contract macro + descriptor; wire them into the resolution pass
   so propagating `f32` outputs actually resolve to Value when all `f32` inputs are Value.
5. **Operator sweep.** Migrate every operator to `io.in`/`io.out`; per op choose `f32` vs
   `f32_buffer` ports and the output tag. Priority cases to validate the model: `add`/`mul`
   (propagating, Value fast-path), `filter` (`f32_buffer` audio + `f32` cutoff match),
   `oscillator` (`f32_buffer` out), `euclid` (always-Value gate via `set(f,1)`/`set(f+1,0)`),
   `voicer` (Event in, `f32`/`f32_buffer` out). Delete old verbs once the sweep is complete.
6. **Coercion enforcement** at plan time: legal Value→Signal widening; hard error + clear message
   on every other cross-form wire; document the named converter ops needed.
7. **Boundary + addresses.** Move `address` out of the internal path into boundary operators.
8. **Docs + schema sweep** (`sync-docs`), re-bless golden snapshots, update authoring + skills.
