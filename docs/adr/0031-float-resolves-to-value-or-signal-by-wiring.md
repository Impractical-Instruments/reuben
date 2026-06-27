# A `Float` port declares Value or Signal; a Value materializes into a Signal only at a Signal input

> Filename keeps the older `-by-wiring` slug to avoid breaking the cross-references in
> ADR-0028/0029/0030; the decision is now **by declaration**, not by wiring. See the Revision note.

## Status

Accepted (2026-06-27). Resolved in a grilling session. **Decision revised the same day** (a second
grilling session, scrubbing the gate/CV ports) to replace the plan-time **propagation** mechanism
with **declared port forms** — see [Revision](#revision-2026-06-27). Supersedes the **"`Float` is
always a buffer underneath"** / static-read-view / always-materialize decisions of
[ADR-0028](0028-one-input-shape.md) and [ADR-0030](0030-osc-as-all-data-one-message-type.md), and
the **"all numeric operands are materialized `Float`"** rule of
[ADR-0029](0029-math-family-dense-float-one-file-per-op.md). Builds on — does **not** retract —
0030's foundation: one `Message = { address, frame, Arg }`, one closed `Arg`, one per-port ZOH
latch, `Signal` = a Message whose `Arg` is a `Buffer`.

## Revision (2026-06-27)

The original Decision below resolved each `Float` port's form with a **plan-time topological
propagation pass**: outputs carried a denseness tag, and the form (`Value` vs `Signal`) of every
`f32` port was *computed* from its inputs. Scrubbing the gate/CV ports against the real DSP showed
that pass to be the wrong kind of cleverness — the forward solver, the feedback back-edge rule, and
the `F32In`/`F32Out` two-arm read API were all complexity in service of *never asking the author to
choose a form*. That "magic" was the single thing making the model hard to hold.

**The form is now declared per port, not propagated.** `f32` = Value, `f32_buffer` = Signal, fixed
at authoring. The planner's only form job is a **local per-wire check** (materialize a Value into a
Signal input; hard-error a Signal into a Value input). No solver, no tags, no sum-type read API.
Math/shaper ops ship as **explicit per-form variants** (value-math and signal-math nodes) rather
than one node that adapts. The sections below are written to the revised model; the
[Considered alternatives](#considered-alternatives) record propagation as the rejected path.

## Context

[ADR-0030](0030-osc-as-all-data-one-message-type.md) collapsed seven carriers into one Message
stream read three ways. It kept one performance shortcut from
[ADR-0028](0028-one-input-shape.md): a `Float` is **always materialized into a per-sample buffer**,
and an operator picks a **static** read view (`io.signal` for per-sample DSP, `io.last`/`io.value`
for block-rate) fixed at authoring, *never* conditional on what is wired. Const-folding via a
`varying` hint was offered as an optional optimization on top.

**That "always materialize" was a mistake that slipped through, not a considered decision** — it
was carried forward from 0028's draft without anyone weighing the allocate-and-fill cost against
keeping a `Float` held. This ADR corrects it.

A `Float` that changes rarely — a `cutoff` knob, a `tempo`, a gate that fires twice a second — still
pays a **per-sample price it does not owe**:

- the engine **allocates and fills a `frames`-length buffer every block** for a value that is
  constant across it, and
- any operator reading it per-sample does **48k iterations/second** of work whose answer changes,
  say, twice.

The deeper miss: `Float` is the only `Arg` type for which "dense vs held" is left undecided and then
resolved by *always picking dense*. Every other type already lives in its natural form —
`Enum`/`Harmony` are held (latch, block-sliced), `Note` is a sparse event stream. `Float` should
too: **held (a Value) for sparse/control data, dense (a Signal) only where per-sample resolution is
musically required.** Which one a given port is is **knowable by the author** — it is a property of
what the port *is* (a knob, a gate, an audio wire), not something to discover from the graph.

This is pre-alpha; breaking changes are acceptable. The code is mid-migration toward 0030 —
`plan.rs` still carries `PortKind { Dense, Held, Stream }` with `port_kind()` mapping
`F32 => Dense` (`crates/reuben-core/src/plan.rs`), and `Io` still exposes
`signal`/`last`/`stream`/`varying`/`signal_mut`/`emit` (`crates/reuben-core/src/operator.rs`). This
ADR settles the `Float` story so the migration finishes against the right target.

## Decision

### Three runtime forms

A wire carries one of **three forms**, replacing `PortKind { Dense, Held, Stream }`:

| Form | latched? | valued | domain | examples |
|---|---|---|---|---|
| **Value** | yes | single | sparse (Message-rate) | `f32`, `Enum`, `Harmony`, `i32`, `Str` |
| **Event** | no | multi | sparse (Message-rate) | `Note`, a note generator's output |
| **Signal** | — | per-sample | dense (audio-rate) | oscillator/filter audio, an LFO, a phasor |

`PortKind` becomes **`{ Signal, Value, Event }`**. The two sparse forms fall out of two independent
axes — *latched?* and *single-valued?* — whose only sensible combinations are **Value** (latched ∧
single) and **Event** (unlatched ∧ multi). The other two are nonsense, so the set is closed at three.

**Slicing is derived, not declared.** Block-slicing makes a piecewise-constant **latched single
value** read as a constant within a `process()` call. So **slice = latched ∧ single-valued = Value.**
Events are not sliced (the consumer reads frame-stamped events directly); Signals are not sliced
(dense by definition). The engine derives slice-or-not from the value type `T`.

### A port *declares* its form: `f32` (Value) or `f32_buffer` (Signal)

The form is **fixed at authoring by the port's value type**. It is not inferred from the graph and
not resolved by a plan-time pass. The numeric type is the only one with a choice, and the author
makes it by writing one keyword or the other:

| Declared type | Form | Used for |
|---|---|---|
| **`f32`** | **Value** (sparse / held; no buffer; compute-on-change, ZOH between) | knobs read block-rate (`tempo`, `cutoff`-of-a-knob-only op), gates & triggers, event-driven numeric outputs |
| **`f32_buffer`** | **Signal** (dense per-sample buffer) | values that must vary smoothly per-sample — `filter.cutoff`, `oscillator.freq`, audio wires |
| **`enum(T)` / `harmony`** | Value only | `mode`, `waveform`, tonal context |
| **`note`** | Event only | `Note` ports |

`f32_buffer` replaces the old `buffer` keyword (and `PortType::Buffer`, `Arg::Buffer`) throughout,
leaving room for future `Signal<T>` buffer kinds without the name lying. The `float` keyword retires
in favour of `f32`. `control`/`signal` capability keywords are never introduced — the type *is* the
declaration.

**Choosing the form is a one-keyword authoring decision, made per port from what the port is:**

- Declare **`f32_buffer`** when stepped/held values would sound wrong — a continuously modulatable
  control (`filter.cutoff` swept by an LFO) or genuine per-sample data (`oscillator.freq` for
  smooth pitch, any audio wire). A Value source wired in is **materialized** (below), so a constant
  still works — it just costs a (constant) buffer.
- Declare **`f32`** when the data is sparse, held, or event-like — a block-rate knob (`tempo`,
  `steps`), a gate/trigger input (`euclid.clock`, `envelope.gate`, `sample.gate`), a pitch latched
  once per hit (`sample.freq`), or an event-driven numeric output (`euclid.gate`,
  `voicer.freq`/`gate`, `clock.gate`).

### The planner's only form job: a local per-wire check

There is **no topological forward pass, no propagation, no denseness tags.** At each wire the
planner compares the two declared forms and does exactly one of:

- **Value → Signal input** → **materialize**: ZOH the latched Value into the destination's block
  buffer at its change frame. This is the **one implicit coercion** (0030's single bridge), now
  firing *only* at a genuine `f32_buffer` input fed by a Value — a constant `cutoff`, or
  `voicer.freq` (Value) feeding `oscillator.freq` (Signal).
- **Signal → Value input** → **hard plan error.** No implicit sample-and-hold (which sample?).
  Crossing this direction needs an **explicit sig→val converter** — an envelope follower, a
  quantizer — which does not exist yet. So you currently *cannot* wire, e.g., an envelope output
  (Signal) into a gate input (Value). **That is by design;** support for such patches lands with the
  converter ops, later.
- **Value → Value**, **Signal → Signal** → direct.
- **Event ↔ Value**, **Event ↔ Signal** → hard plan error (need an explicit latch / change-detect /
  converter op).

Buffer allocation falls straight out: **allocate an `f32_buffer` only for a declared-Signal port or
a materialized Value→Signal edge.** Value ports get a **latch slot** only. Block-slice a node at the
union of its Value inputs' change frames.

### Math / shaper ops ship per form

`add`, `mul`, `power`, `map`, … come in **explicit per-form variants**, not one node that adapts.
The two variants are named with a **`_value` / `_signal` suffix** — `add_value` / `add_signal`,
`mul_value` / `mul_signal`, `power_value` / `power_signal`, … — and the bare name (`add`) retires so
the form is never ambiguous at the call site:

- a **`*_value`** node has `f32` ports (sparse arithmetic on held controls / events),
- a **`*_signal`** node has `f32_buffer` ports (per-sample audio arithmetic — mixing, VCA).

There is no `add` that resolves its form from its inputs (that *was* propagation). The existing
dense math ops are **renamed to `*_signal`**; the **`*_value`** family is net-new, expected to be
used heavily and to grow (sparse control math is common), and is authored as part of the operator
sweep rather than discovered by the engine.

### Read / write API: direct accessors, no sum type

Because a port's form is fixed at declaration, the operator reads it with the **matching direct
accessor** — there is no runtime `match` on form:

```rust
let cutoff = io.in_signal(IN_CUTOFF);          // f32_buffer  -> &[f32]   (a Value source was materialized upstream)
let steps  = io.in_value::<f32>(IN_STEPS);     // f32         -> f32      (held, block-sliced)
let mode   = io.in_value::<FilterMode>(IN_MODE);
for ev in io.in_event::<Note>(IN_NOTES) { ... } // note       -> Event iterator

io.out_value::<f32>(OUT_GATE).set(frame, 1.0);  // f32 output -> MsgWriter (deduped, last-wins, addressless)
let buf = io.out_signal(OUT_AUDIO);             // f32_buffer output -> &mut [f32]
```

- **`MsgWriter::set(frame, value)`** is the single Value-write primitive (lowers to today's
  `Emit → Event → latch`): **deduped** against the current latch (a no-op change emits nothing — the
  wire stays genuinely sparse) and **last-write-wins** per frame. `euclid` calls it twice per fired
  step: `set(f, 1.0)`, `set(f + 1, 0.0)`.
- The **`F32In` / `F32Out` sum types and `match io.in::<f32>` are not introduced** — they existed
  only to handle a port that could be either form at runtime, which declared forms eliminates.
  `varying()` stays **deleted** (no propagation, nothing to const-fold against).
- The old verbs (`signal`/`last`/`stream`/`varying`/`signal_mut`/`emit`) are **deleted, no compat
  wrappers**.

### Addresses are a boundary concept

Internal wires route by connection (`src_port → dst_port`), so internal Value/Event writes are
**addressless** — `MsgWriter::set(frame, value)` carries no address, and the `address` field drops
from the internal `Emit`/hot path. This finishes what 0030 started ("address … never internal
dispatch"). Addresses survive only at OSC/MIDI **boundary operators**, which map address ↔ port.

### Locked port-form decisions (gate/CV scrub)

The scrub that produced the [Revision](#revision-2026-06-27) settled the contested numeric ports.
These are the authoritative declarations the operator sweep implements:

| Operator · port | Dir | Declared | Form | Why |
|---|---|---|---|---|
| `clock.phase` | out | `f32_buffer` | Signal | a true [0,1) ramp — changes every sample |
| `clock.gate` | out | `f32` | Value | a trigger = sparse edges; emitted from inside the phasor loop |
| `euclid.gate` | out | `f32` | Value | two emits per fired step |
| `voicer.freq` | out | `f32` | Value | held pitch; the op already builds a sparse change-list |
| `voicer.gate` | out | `f32` | Value | on/off only at note edges |
| `envelope.cv` | out | `f32_buffer` | Signal | a per-sample ADSR ramp |
| `oscillator.freq` | in | `f32_buffer` | Signal | stepped pitch-mod sounds bad; `voicer.freq` materializes in |
| `filter.cutoff` | in | `f32_buffer` | Signal | swept by an LFO; a constant materializes |
| `euclid.clock` | in | `f32` | Value | reads `clock.gate` edges; **a Signal source errors** |
| `envelope.gate` | in | `f32` | Value | trigger; **a Signal source errors** (no env→gate yet, by design) |
| `sample.freq` | in | `f32` | Value | latched once per hit — stepped is correct (unlike osc.freq) |
| `sample.gate` | in | `f32` | Value | rising-edge trigger |
| `clock.tempo`/`division`, `euclid.steps`/`pulses`/`rotation`, `envelope` ADSR, `sample.root`/`gain`/`start`/`channel` | in | `f32` | Value | block-rate knobs |

The discriminator throughout: **does the value vary per-sample in a musically required way?** Yes →
`f32_buffer` (Signal). No (sparse / held / latched-per-hit / a trigger) → `f32` (Value).

## Considered alternatives

- **Plan-time propagation (this ADR's own original mechanism — revised out).** Outputs carried a
  denseness tag and a topological pass resolved each `f32` port to Value/Signal from its inputs
  (`f32` out = Value iff all `f32` inputs Value, else Signal), with a feedback back-edge pinned to
  Signal and an `F32In`/`F32Out` two-arm read API. **Rejected on reflection:** the forward solver,
  the cycle special-case, and the sum-type API were all complexity spent to avoid ever asking the
  author which form a port is — and that avoidance was the single thing making the model hard to
  hold and to implement. The form is a one-keyword authoring fact; computing it is misplaced cleverness.
- **Author declares the form per port (now chosen).** This was *originally rejected* here as "a
  false choice on every modulatable control — declare `cutoff` held and you can't sweep it with an
  LFO." **That reasoning does not bind:** a port that must be modulatable is simply declared
  `f32_buffer` (Signal), which accepts both a Signal source and a materialized constant — no lost
  capability, no second variant. The library *does* fork for math ops (value-math vs signal-math
  nodes), and that is a **feature**: the value-math family is wanted and will be large. Forking a
  handful of arithmetic ops is far cheaper than a graph-wide solver.
- **A `control` capability keyword (Value-or-Signal).** Rejected as redundant: "which form" is the
  `f32` vs `f32_buffer` type itself. The capability is the type, not an extra thing to declare.
- **A single always-per-sample read view (`F32In` iterator that yields per-sample regardless).**
  Rejected: it throws away the held fast-path — back to 48k iterations on a constant.

## Consequences

- **Breaking, engine-wide.** `PortKind { Dense, Held, Stream } → { Signal, Value, Event }`;
  `port_kind()` and the materialize-always path in `plan.rs`/`render.rs` are replaced by a
  **per-wire form checker** (materialize Value→Signal; hard-error Signal→Value and Event mismatches)
  — *not* a topological solver. `Io` exposes `in_value`/`in_signal`/`in_event`/`out_value`/
  `out_signal` in place of `signal`/`last`/`stream`/`varying`/`signal_mut`/`emit`. `varying()` is
  gone. No `F32In`/`F32Out`. The golden descriptor snapshot and generated instrument schema are
  re-blessed.
- **Buffers are allocated only for declared-Signal ports and materialized Value→Signal edges.** A
  graph of held `Float`s (a `tempo` feeding a clock, a constant `cutoff` that materializes once)
  allocates the minimum, and Value ports do **zero** per-sample work.
- **Math/shaper ops split into `*_value` (`f32`) and `*_signal` (`f32_buffer`) variants.** The
  existing dense ops are renamed to `*_signal`; the `*_value` family is **net-new** authoring.
  0029's scalar-fn + dense-shell structure survives in the `*_signal` variants. The bare names
  (`add`, `mul`, `power`) retire — every reference becomes suffixed (instruments re-blessed).
- **`Float` is held by default for control data, dense by author's choice for per-sample data** —
  `add_value(const, const)` stays a Value (no buffer); audio mixing uses `add_signal`.
- **`f32_buffer` replaces `Buffer`** as the type/keyword name (`PortType`, `Arg::Buffer`, contract
  macro, schema). `float`/`buffer`/`control`/`signal` keywords retire; ports are declared by value
  type (`f32`, `f32_buffer`, `enum(T)`, `harmony`, `note`).
- **`MsgWriter` is deduped + frame-addressed**; the internal `Emit`/`Event` path drops `address`.
  Addresses live only in boundary operators.
- **Signal → Value is a hard error with no implicit bridge.** Patches that need it (envelope →
  gate, audio → control) are **unsupported until explicit sig→val converter ops exist** — a
  deliberate, documented gap, not an oversight.
- **Feedback cycles are out of scope.** They remain a hard `PlanError::Cycle` (Kahn sort in
  `plan.rs`), per [ADR-0009](0009-graph-lifecycle.md)'s unit-delay deferral. The per-wire checker
  assumes a DAG; revisit when feedback lands.
- **Amends** [ADR-0011](0011-message-delivery-and-timing.md) (block-slicing now serves Value ports —
  latched ∧ single-valued — not `Float`), and the materialize bridge of
  [ADR-0030](0030-osc-as-all-data-one-message-type.md) (fires only at an `f32_buffer` input fed by a
  Value, never speculatively).
- **`map` stays event-domain** for now (its Float reframe is staged with the instrument migration,
  per [ADR-0029](0029-math-family-dense-float-one-file-per-op.md)); it is untouched here.
- **Authoring contract:** `docs/agents/authoring.md`, `ARCHITECTURE`, `CONTEXT.md`, and the
  create-operator skill are swept to teach: **declare each port `f32` (Value) or `f32_buffer`
  (Signal)** by what it is; the direct accessors (`in_value`/`in_signal`/`out_value`/`out_signal`);
  value-math vs signal-math nodes; the one legal implicit coercion (Value→Signal) and the hard
  error on its reverse.

## Implementation plan (sequenced; each step compiles + tests green before the next)

A separate thread from the decision, sketched here so the migration is ordered, not discovered. The
detailed per-step oracle, fixtures, and operator waves live in
[0031-impl-prep.md](0031-impl-prep.md).

1. **Forms + per-wire checker, behind the current API.** Introduce
   `PortKind { Signal, Value, Event }` and the **wire-checker** in `plan.rs`: allocate an
   `f32_buffer` only for a declared-Signal port or a materialized Value→Signal edge; Value ports get
   a latch slot; block-slice at Value-input change frames; **materialize** Value→Signal and
   **hard-error** Signal→Value / Event mismatches. No propagation, no feedback back-edge rule. Keep
   the old `Io` verbs working over the new allocation. Bless the descriptor snapshot.
2. **`Arg::Buffer → Arg::f32_buffer`** rename + `PortType` rename + contract-macro keyword change
   (`buffer → f32_buffer`, drop `float` in favour of `f32`). Mechanical, repo-wide; re-bless schema.
3. **New `Io` API.** Add `in_value`/`in_signal`/`in_event`/`out_value`/`out_signal` + `MsgWriter`
   (deduped, frame-addressed, addressless). **No** `F32In`/`F32Out`, no `match`. Keep old verbs
   temporarily.
4. **Declare port forms in the contract** per the [locked table](#locked-port-form-decisions-gatecv-scrub):
   each numeric port is `f32` or `f32_buffer`. Pure authoring — the engine does no resolution.
5. **Operator sweep.** Migrate every operator to the direct accessors and its declared forms.
   Priority cases that validate the model: `filter` (`f32_buffer` audio + `f32_buffer` cutoff),
   `oscillator` (`f32_buffer` freq + out), `euclid` (`f32` Value gate via `set(f,1)`/`set(f+1,0)`,
   `f32` clock input reading edges), `voicer` (Event in, `f32` Value freq/gate out), `envelope`
   (`f32` Value gate in, `f32_buffer` cv out). **Author the new `*_value` math nodes** (`add_value`/
   `mul_value`/`power_value`); rename the existing dense math ops to `*_signal`. Delete old verbs
   once the sweep is complete.
6. **Coercion enforcement** at plan time: legal Value→Signal materialization; hard error with a
   clear message on Signal→Value and on every Event mismatch; document the named converter ops the
   error points at.
7. **Boundary + addresses.** Move `address` out of the internal path into boundary operators.
8. **Docs + schema sweep** (`sync-docs`), re-bless golden snapshots, update authoring + skills.
