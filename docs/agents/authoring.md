# Authoring: Operators, Instruments, Rigs

The grounding doc for building reuben — the concrete code contract behind the conceptual
narrative in [ARCHITECTURE.md](../ARCHITECTURE.md). Capitalized terms (Operator, Voice,
Plan…) are defined in [CONTEXT.md](../../CONTEXT.md). The ADRs are the source of truth;
this doc tells you where the contract lives in code and how to extend it.

## The recursive model

One concept at every scale ([ADR-0003](../adr/0003-recursive-composition.md)): a graph of
nodes with typed ports.

- **Operator** — the smallest unit of behavior; does one simple thing.
- **Instrument** — a named subgraph of Operators exposing boundary ports; reusable inside
  another Instrument *as if it were an Operator*.
- **Rig** — a full playable system: Instruments wired with routing.

Nesting is an authoring concept only; at runtime everything inlines into one flat graph.

## One `Input`, one `Arg` type ([ADR-0030](../adr/0030-osc-as-all-data-one-message-type.md))

Every functional input an operator consumes is **one `Input`**, declared once, carrying one
piece of typed data — its **`Arg`** type, drawn from one closed, central enum. How the value is
read follows from the `Arg` type plus the read verb; whether a numeric port is a held **Value**
(`f32`) or a dense **Signal** (`f32_buffer`) follows from which keyword it declares (ADR-0031).
Outputs carry an `Arg` the same way. (The ADR-0028 **`shape`** axis is **retired** — the axes now
are the port's `Arg` type and its declared form.)

The read/write surface is **two verbs over typed handles** ([ADR-0037](../adr/0037-typed-port-handles.md),
extending ADR-0031): `io.read(IN_X)` and `io.write(OUT_X)`. The contract macro emits each port's
const as a typed handle — `In<form>` / `Out<form>` — whose *type* fixes the read/write shape and
whose value carries the declared default, so a wrong-form read **does not compile** and a held
read's fallback **is** the contract default (no second literal to drift).

| `Arg` type (form) | what it is | `io.read(IN)` / `io.write(OUT)` |
|---|---|---|
| **`f32_buffer`** (a *Signal*, `In<SignalF32>`) | dense per-sample audio / CV / control — the one buffer payload | read: `&[f32]`, **always exactly `io.frames()` samples** (the buffer-presence invariant — index directly; + `io.varying(IN)`) · write: `&mut [f32]` |
| **`f32`** (a held *Value*, `In<Held<f32>>`) | a number — freq, cutoff, amp, a contour; owns a default, latched and read once per (sub)block | read: `f32`, defaulted to the declared default · write: `MsgWriter` (`.set(frame, v)`) |
| **enum** (a *vocab* type, a Value, `In<Held<E>>`) | a named discrete choice — `FilterMode`, `Waveform` | read: the real Rust enum (not an index), defaulted to its `#[default]` variant |
| **`Harmony`** (vocab struct, a Value, `In<Held<Harmony>>`) | the tonal-context struct: `root`/`scale`/`chord` + resolvers `hz()`/`snap()`/`chord_tone()` | read: `Harmony`, defaulted to C-major 12-TET (`Harmony::DEFAULT`) · write: `MsgWriter` (`.set(frame, h)`) |
| **`Note`** (vocab struct, an Event, `In<Event<Note>>`) | a pitch/velocity event | read: `EventStream<Note>` of `Stamped<Note>` (`.frame`, `.payload`) · write: `EventWriter` (`.emit(frame, note)`) |

A port's **form** is one of three — **Signal** (`f32_buffer`), **Value** (`f32`/enum/`Harmony`, a
held latch read once per slice), **Event** (`Note`, a sparse frame-stamped stream) — and follows from
the declared `Arg` type (`PortKind` in `plan.rs`). Reading older code: **Signal** = `f32_buffer`;
**param** = an `f32` Value or held enum; **Context** = `Harmony`; **Message events** = `Note` (the
ADR-0028 `shape`/temporality axis is gone). A runtime integer is a rounded `f32` or an enum; `I32` is
an OSC primitive `Arg`, but no operator declares an `Int` port.

### Form is declared, not inferred: `f32` is a held Value, `f32_buffer` is a Signal

The author picks a numeric port's form by which keyword it declares
([ADR-0031](../adr/0031-float-resolves-to-value-or-signal-by-wiring.md)):

- **`f32` → a held Value.** A latched scalar read once per block-slice with `io.read(IN)`. The
  engine block-slices at each change frame, so the read is sample-accurate without a buffer. It
  seeds its latch from override-or-default; unwired it reads the declared default (carried by the
  handle itself, ADR-0037). No buffer allocated.
- **`f32_buffer` → a Signal.** A dense per-sample buffer read with `io.read(IN)` — audio, CV,
  or any *swept* control (a filter cutoff an LFO modulates). A meta default materializes a constant
  buffer when unwired/knob-set; an unwired *bare* buffer materializes **silence** — so the read is
  a real length-n buffer in every case (the **buffer-presence invariant**, ADR-0037): index
  `io.read(IN)[i]` directly, no `.get(i).unwrap_or(..)` guard.

A cheap **`varying: bool`** rides alongside a Signal read (`io.varying(IN)`): `false` when a
materialized input held unchanged this block, `true` when dense or changed. A const-folding op (a
filter recomputing biquad coefficients only when `cutoff` moves) opts in; a naive op ignores it and
reads `io.read(IN)[i]` — always correct.

So form follows the processing model: per-sample DSP (osc, filter, `mul_f32_signal`, the envelope's
`cv`) declares `f32_buffer` and reads a slice; block-rate controls (a clock's `tempo`, a sequencer's
`length`, a gate edge) declare `f32` and read the held value.

### Wiring across forms: one legal coercion, the rest hard errors

Each wire is checked **locally** at Instantiate against the two ports' forms (`check_wire_forms` in
`plan.rs`, surfacing `PlanError::FormMismatch`). The rules:

- **like → like** (`Signal→Signal`, `Value→Value`, `Event→Event`) connects directly.
- **`Value → Signal`** is the **one implicit coercion**: the held Value materializes (ZOH) into a
  buffer at the Signal input, a mid-block change written at its frame (sample-accurate, one
  `process()`). This is the canonical `voice.freq`(Value) → `osc.freq`(`f32_buffer`) bridge.
- **`Signal → Value`** is a **hard error** — there is no implicit sample-and-hold; wire an explicit
  converter (an envelope follower / quantizer). Into an enum Value it's equally illegal (an enum
  takes a discrete choice, not a per-sample signal).
- **`Event` mismatched** against a Signal/Value is an error (needs an explicit latch / change-detect).

Every cross-*type* crossing still needs an operator: `f32 → enum` is a quantizer; `f32 → Note` is a
threshold/trigger; `slew`/`glide` are `f32 → f32` shapers (the `m2s` gap-filling modes).

### `Constant` — instantiate-time configuration, not an `Input`

A **`Constant`** configures an operator *instance* at instantiate time and never changes on the
data path. The boundary is precise: **a value is a `Constant` iff changing it would rebuild the
graph.** The canonical (and today only) case is the Voicer's `voices` — it sets the voice-pool size,
hence how many voice sub-patches are instantiated, so it can't be a runtime value. A `Constant` is
declared with the contract's **`constant: <param>`** keyword (ADR-0032) and lives in the patch's
`config` block, not `inputs`.

**`Arg` type does not decide `Constant`-vs-`Input`.** `mode` (Lp/Hp/Bp) and `waveform` (Sine/Saw)
are enums, but changing them rebuilds nothing — only which coefficients run — so they are **runtime
enum inputs**, switchable live over OSC. Only genuinely topology-fixing values are `Constant`s.

## The Operator contract (`crates/reuben-core/src/operator.rs`)

Operators are authored **single-Voice** ([ADR-0010](../adr/0010-single-lane-operators.md)):
you write one mono, single-Voice stream a (sub)block at a time. Polyphony is not a per-operator
fan-out — the **Voicer** hosts N voice sub-patches and sums them ([ADR-0032](../adr/0032-voicer-hosts-voice-subpatches.md)),
so an operator never carries per-Voice copies. The trait is three core methods (plus optional
lifecycle hooks):

```rust
pub trait Operator: Send {
    /// Static self-description (ports + metadata). Drives serialization, connection
    /// checking, good-button controls, and AI grounding.
    fn descriptor() -> Descriptor where Self: Sized;

    /// Process exactly one (sub)block. Must not allocate.
    fn process(&mut self, io: &mut Io);

    /// Fresh-state instance of the same type.
    fn spawn(&self) -> Box<dyn Operator>;

    /// Receive decoded resources after construction. Default no-op;
    /// only resource-bearing operators (the sample player) override it.
    fn bind_resources(&mut self, store: &Arc<ResourceStore>, refs: &ResolvedRefs) {}
}
```

Two optional lifecycle hooks support the Voicer ([ADR-0032](../adr/0032-voicer-hosts-voice-subpatches.md)),
both default no-ops: `bind_voices(Vec<Graph>)` receives the N built voice sub-patches at load, and
`on_instantiate(&AudioConfig) -> Result<(), PlanError>` runs per node from `Plan::instantiate` (the
one place with the audio config) so the Voicer can build each voice's sub-`Plan` + arena off the hot
path.

- **`descriptor()`** — see below. The single source of an operator's ports and metadata.
- **`process(io)`** — the only realtime path. **Allocation-free.** Read inputs, write outputs
  through the `Io` view, by the contract's typed handles (ADR-0037) — the handle's form decides
  the shape; a wrong-form access does not compile:
  - `io.read(IN) -> &[f32]` on an `In<SignalF32>` — read a **`f32_buffer`** (Signal) input, or the
    materialized buffer of a Value source wired into it. Exactly `io.frames()` samples, always
    (buffer-presence invariant) — index directly. `io.varying(IN)` is the change hint.
    `io.write(OUT) -> &mut [f32]` fills a `f32_buffer` output in place.
  - `io.read(IN) -> f32` on an `In<Held<f32>>` — read a held **`f32`** Value (the block-rate scalar
    view, a clock's `tempo`, a gate edge), defaulted to the contract default the handle carries.
    An enum handle (`In<Held<Waveform>>`) reads the real *vocab* type, constant for the
    (sub)block: `io.read(IN_WAVEFORM) == Waveform::Saw`. No `enum_index`/`from_index` on the hot
    path. `io.write(OUT) -> MsgWriter` writes a held Value: `.set(frame, v)` is deduped (an
    unchanged value emits nothing) + last-write-wins per frame.
  - `io.read(IN) -> EventStream<Note>` on an `In<Event<Note>>` — read **`Note`** events (Voicer,
    sequencer): a zero-copy iterator of `Stamped<Note>` (`.frame` segment-relative, `.payload` the
    decoded `Note`). `io.write(OUT) -> EventWriter` writes events: `.emit(frame, payload)` is
    **append-only** (no dedup, no last-write-wins — a chord's tones at one frame all survive).
    Internal wires are **addressless** — routed by connection, not name
    ([ADR-0014](../adr/0014-internal-message-graph.md), ADR-0031 step 7). See `sequencer.rs` /
    `snap.rs`.
  - `io.read(IN) -> Harmony` on an `In<Held<Harmony>>` — read the latched tonal **`Harmony`**
    (key/scale/chord + resolver `hz`/`snap`/`chord_tone`), constant for the (sub)block, default
    C-major/12-TET when unwired. A `harmony` Operator writes the other side via
    `io.write(OUT_HARMONY) -> MsgWriter` (a Harmony is a held Value, dedup+LWW is correct).
    The Voicer and `snap.rs` read it; `harmony.rs` emits it.
- **`spawn()`** — usually `Box::new(Self::new())`. Resets per-Voice state only. A resource-bearing
  operator carries its binding (the `Arc<ResourceStore>` + resolved handle) forward while resetting
  playback state, so every Voice shares the decoded data — see `sample.rs`.
- **`bind_resources(store, refs)`** — the two-phase-init hook for operators depending on
  **external decoded data** ([ADR-0016](../adr/0016-sample-player-and-resource-store.md)). The
  loader resolves+decodes the document's `resources` table into a shared `ResourceStore` and calls
  this hook on each node declaring a resource slot. Default no-op. `sample.rs` is the template.

State that must persist across blocks lives on the struct (e.g. an oscillator's phase). Hold an
accumulating phase in `f64` so it doesn't drift over a long session (see `lfo.rs`).

## The Descriptor (`crates/reuben-core/src/descriptor.rs`)

An operator's self-description, separate from `process` — the seat of "good button",
serialization, connection checking, and AI grounding
([ADR-0004](../adr/0004-ai-authorability-first-class.md)).

You declare it **once**, in an `operator_contract!` call
([ADR-0025](../adr/0025-single-source-operator-contract.md)). The macro plants, at module scope,
the **typed `IN_`/`OUT_` handles** (`In<form>`/`Out<form>` consts whose type is the port's form and
whose value carries the declared default — ADR-0037), the `C_*` constant ordinals, **and** an
inherent `fn contract() -> Descriptor` from the same tokens, so handles and descriptor can't drift. An `enum` port **references a shared *vocab* enum** by
name (`enum(FilterMode)`) — it generates no per-op type; the descriptor is single-sourced off the
vocab type's `FilterMode::enum_meta(name)` (ADR-0030). The trait's `descriptor()` delegates:

```rust
crate::operator_contract!(Filter {
    inputs:  { audio: f32_buffer,                              // a Signal (audio/CV) input
               cutoff: f32_buffer { 20.0..=20_000.0, default 1_000.0, "Hz", exp },  // a swept control: Signal + materializing default
               resonance: f32 { 0.0..=1.0, default 0.2, "", lin },  // a held Value control
               mode: enum(FilterMode) },                       // a live-switchable enum (Value)
    outputs: { audio: f32_buffer },
});

// An operator with an instantiate-time Constant declares it in `constants:` (the Voicer):
crate::operator_contract!(Voicer {
    inputs:  { notes: note, harmony: harmony },
    outputs: { audio: f32_buffer },
    constants: { voices: i32 { 1..=32, default 8 } },          // instantiate-time config — rebuilds the graph if changed
    resources: { voice },                                      // an instrument-resource slot (the voice sub-patch)
});

impl Operator for Filter {
    fn descriptor() -> Descriptor { Self::contract() }   // one-liner delegate (ADR-0025)
    fn process(&mut self, io: &mut Io) {
        let mode = io.read(IN_MODE);  // a real Rust enum, defaulted to FilterMode's #[default]
        // per sample: let x = io.read(IN_AUDIO)[i]; let cut = io.read(IN_CUTOFF)[i];  (`varying` lets it const-fold)
        // one buffer loop over io.frames(), writing io.write(OUT_AUDIO)[i] ...
    }
    // spawn ...
}
```

`Arg`-type forms in the macro (each emits the matching `Port::*` constructor):

- **`name: f32_buffer`** — a **Signal** (audio/CV) port with no settable default, e.g. a passthrough
  `audio` in or any per-sample output (`Port::f32_buffer`).
- **`name: f32_buffer { MIN..=MAX, default D, "unit", lin|exp }`** — a **Signal with a materializing
  default**: classifies Signal (an LFO wires straight in) yet unwired/knob-set materializes a constant
  buffer from `default`. Use for *swept* controls (`filter.cutoff`, `oscillator.freq`).
- **`name: f32 { MIN..=MAX, default D, "unit", lin|exp }`** — a **held `f32` Value control** that owns
  its unwired default (`Port::f32`), read once per slice. `"unit"` and the curve are each optional.
- **`min`/`max` range sentinels** — anywhere a range endpoint or `default` takes a literal, `min`/`max`
  stand in for the type-wide `±1e6` bound (`reuben_contract::NUMBER_MIN`/`NUMBER_MAX`, the one
  definition — issue #127): `{ min..=max, default 0.0 }` is an unbounded knob, `{ 0.0..=max, .. }`
  pins a real floor with an unbounded ceiling. In `default`, `max`/`min` resolve to the port's **own**
  declared range edge — `default max` parks an operand at its ceiling as a no-op (see `min`/`max`'s
  `b`) without repeating the number.
- **`name: enum(VocabType)`** — an enum (Value) input naming its shared *vocab* type (`Port::enumerated`
  off `VocabType::enum_meta`); the type's `#[default]` variant is the default.
- **`name: note`** / **`name: harmony`** — `Note` (Event) / `Harmony` (Value) ports (`Port::note` /
  `Port::harmony`).
- **`name: arg`** — a **type-agnostic pass-through** (issue #141): carries *any* `Arg` as a raw
  Event stream (`Port::arg`), read via its `In<Raw>` handle (`io.read(IN)` yields undecoded
  `&Arg` payloads) and re-emitted via the `io.output::<Arg>` primitive.
  **Input-only**, and only for a **pure carrier** — an operator that treats the payload as opaque
  (forward, buffer, drop) and never interprets it; the wired *source* port is the type authority.
  Legality is capability-keyed: any Event or Value source whose type has an **external OSC form**
  wires in — primitives, vocab enums, and any struct vocab type whose converter is registered
  with the boundary (`register_osc_form!` in `boundary.rs`, epic #146; `Note`'s flat form today);
  a `Harmony` source (no OSC form — it registers none; its wire form is deferred to issue #209)
  and a Signal source are rejected at load/plan — audio never crosses the boundary. Inbound is
  asymmetric: external OSC addressed at an `arg` port crosses only as a **single atom**, numeric
  or string (the string joined once `Arg::Str` went `Arc<str>`-backed, issues #206/#207), while
  multi-arg lists drop — so the flat 2-arg Note form the sink *sends* does not round-trip back in
  through an `arg` port; a typed `note` port still decodes it. Today the form of `osc_out.in`,
  the outbound OSC sink.
- **`constants: { name: i32 { LO..=HI, default D } }`** — instantiate-time `Constant`s
  ([ADR-0035](../adr/0035-constants-are-immutable-ports.md), the Voicer's `voices`); the loader
  routes them to the patch's `config` block. Constants keep bare `usize` `C_*` ordinals — they are
  never read in `process`, so they get no handle (ADR-0037).

Other notes:

- An operator with no explicit `type_name:` takes the snake_case of its struct name; pass
  `type_name: "sample"` when they diverge (e.g. `SamplePlayer`).
- **Ports are referenced by name** in the JSON format, not by index — names are the stable
  contract the rig builder wires against. The macro computes the ordinals.
- **No exceptions:** every operator declares its contract through the macro and delegates via
  `Self::contract()` (`output`, the last hand-written descriptor, folded in with ADR-0037 so its
  ports get typed handles too). One operator per module since
  [ADR-0029](../adr/0029-math-family-dense-float-one-file-per-op.md) deleted the old `math.rs`.
- **Pointwise number ops use a higher-level macro.** `add`, `mul`, `power`, `map` are each a single
  `crate::number_operator_contract!(..)` call over one scalar fn, which generates **both carriers**
  (`*F32Value` + `*F32Signal`) — their contracts, `Operator` impls, registration, and a
  defaults-are-data test ([ADR-0033](../adr/0033-number-operator-contract-macro.md)). The criterion:
  an op is macro-eligible iff it is **stateless pointwise** (output sample = fn of this sample's
  inputs only) **and** every operand is a number or held enum mode. `differentiate`/`integrate` are
  **stateful** (they carry state across blocks), so they stay hand-written `operator_contract!` ops
  and are **signal-only** (a value form would shatter their continuous one-sample-`dt` stream).
- **Polyphony** is not a per-operator concern (ADR-0032): there is no Lane fan-out. The **Voicer** is
  a single-Voice operator that hosts N voice sub-patches — a voice is a standalone Instrument
  (instrument-resource, declared `resources: { voice }`) with an `interface { inputs, outputs }`
  boundary (`freq`/`gate` in, `audio`/`active` out). The loader builds the patch `voices` times and
  `bind_voices` them; the Voicer instantiates each into its own sub-`Plan` at `on_instantiate`, drives
  per-voice `freq`/`gate`, and sums their audio. See `voicer.rs` and `instruments/voices/*.json`.

### Enum over the wire: symbol primary, index fallback

An enum input is addressed **by symbol** — its variant name (`/filt/mode "Hp"`, `"mode": "Hp"`):
the human-legible, refactor-stable form, and what an OSC string carries. A bare **integer index**
(`/filt/mode 1`) is accepted as a **fallback**, in range. Resolution lives in one place —
`EnumMeta::resolve` — single-sourced with the shared *vocab* enum type's `VARIANTS`/`from_symbol`
(both generated by `#[derive(ArgValue)]`). An unknown symbol or out-of-range index is an **error** —
it never silently snaps to the default.

## Adding an Operator

1. **Create** `crates/reuben-core/src/operators/<name>.rs` — a struct + `impl Operator`.
   Declare the contract once with `crate::operator_contract!(..)` and delegate
   `fn descriptor() -> Descriptor { Self::contract() }`. Follow `lfo.rs` (simplest source op),
   `filter.rs` (`F32` controls with defaults + an enum), or `delay.rs` (input + state) as templates.
   (`reuben scaffold-operator` writes the skeleton — see the [create-operator
   skill](../../.claude/skills/create-operator/SKILL.md).)
2. **Wire the module** in `crates/reuben-core/src/operators/mod.rs`: `pub mod <name>;`
   and `pub use <name>::<Type>;`.
3. **Self-register** by adding one line at the operator's module top level, after its
   `impl Operator` block: `crate::register_operator!(<Type>);` — a compile-time `inventory`
   submission `Registry::builtin()` gathers ([ADR-0024](../adr/0024-compile-time-operator-registration.md)),
   so there is **no central list to edit**. (`grep -rn register_operator! operators/` is the census.)
4. **Regenerate the schema** so JSON validation knows the new type/inputs:
   ```sh
   cargo run -p reuben-core --example gen_schema
   ```
   Commit the updated `crates/reuben-core/schema/instrument.schema.json`. The
   `committed_schema_is_in_sync` test fails if it's stale.
5. **Test** in the operator module, test-first, with
   [`OpDriver`](../../crates/reuben-core/src/op_driver.rs) — it drives your operator through the
   **real** engine (`Plan::instantiate` + `Renderer::step_node`), so a test can never drift from how
   the engine actually seeds and steps a node. Address ports by the generated `IN_*` / `OUT_*`
   typed handles (every driver verb takes a handle or a bare index — `PortIndex`):
   - `set(IN_X, v)` — a held control (scalar / enum / `Harmony`) or a constant audio-in (sticky/ZOH).
   - `push(IN_X, frame, note)` — a transient `Note` event at a global frame.
   - `drive(IN_X, &buf)` — a time-varying audio-in.
   - `bind("slot", sample_buffer)` — a decoded resource.
   - `render(n)` then `output(OUT_X)` / `emits()` — run `n` frames (as real 128-frame blocks,
     threading state) and read a Buffer output / the emitted Messages. `spawn()` gives a driver over
     a fresh `Operator::spawn` copy.

   At minimum cover output correctness, state continuity across blocks (`render` a length that spans
   several 128-frame blocks — the engine owns the slicing, so there is no manual "whole vs split" to
   build), and that a `spawn()`ed copy starts fresh. The four shapes have exemplars: `lfo.rs` (held
   `set` + buffer `output`), `snap.rs` (`push` + `emits`), `delay.rs` (`drive` + `output`), `sample.rs`
   (`bind`).

Embedders can add their own types without touching the core via `Registry::register` — the
seam for the "agents author new Operators in Rust" goal ([ADR-0004](../adr/0004-ai-authorability-first-class.md)).

## The Instrument format (`crates/reuben-core/src/format.rs`)

An Instrument is plain JSON data ([ADR-0028](../adr/0028-one-input-shape.md); **format v3**
since [ADR-0043](../adr/0043-surface-docs-decouple-presentation-from-instruments.md)): `nodes` (operator
`type` + `address`, plus an `inputs` map, an optional `config` block, and optional `doc`) and
an optional **`interface`** block — the graph's one boundary, everything that crosses its edge
(see below). There is **no top-level `connections` array** and **no per-node `params` map**
(both fold into `inputs`), and **no anonymous master `outputs` array** (v1-only — it dissolved
into named `interface.outputs` entries; the loader migrates old documents).

Each entry in a node's **`inputs`** map is one of:

- a **literal** — `"resonance": 0.4` (an `F32` control default) or `"mode": "Hp"` (an enum by symbol);
- a **wire-ref** — `{ "from": "/osc.audio" }`, or the sole-output sugar `{ "from": "/osc" }` when
  the source has exactly one output.

`"cutoff": 1000` and `"cutoff": { "from": "/lfo.audio" }` target the **same slot**. A node's
**`config`** block holds its `Constant`s (`{ "voices": 8 }`).

```json
{
  "type": "filter", "address": "/filt",
  "inputs": {
    "audio":     { "from": "/osc.audio" },
    "cutoff":    { "from": "/lfo.audio" },
    "resonance": 0.4,
    "mode":      "Hp"
  }
}
```

`format::load` resolves types via a `Registry`, applies literals/config, resolves wire-refs to
edges (checking `Arg` types), and returns a `Graph`. Loading is an authoring step — portable core,
never the audio thread. Errors are specific: `UnknownInput`, `BadInputValue`, `TypeMismatch`,
`ConstantInInputs` (a `Constant` placed in `inputs`), `UnknownConfig`, `AmbiguousWire`. See
`instruments/*.json` for worked examples.

### The `interface` block: named pipes at the boundary ([ADR-0038](../adr/0038-interface-pipes-and-the-device-layer.md))

`interface.inputs` / `interface.outputs` entries are **named pipes** — the single boundary
mechanism at every graph level, with the wiring direction **flipped** relative to the v1
target-pointing form (no entry points inward anymore):

- An **input pipe mints an address in the flat node namespace** (entry `in` → node `/in`; a
  collision with a real node is the fatal `DuplicateAddress`) and behaves like a source node:
  internal consumers wire from it with ordinary wire-refs (`"audio": { "from": "/in" }`),
  fan-out free. Because nothing is pointed at, **the entry declares its own `Arg` type** —
  `"type"`: `"f32_buffer"`, `"f32"`, `"note"`, `"harmony"`, or a vocab enum name — enforced
  against every consumer wire by the ordinary pass-2 wire check. A numeric pipe owns
  engine-enforced `default`/`min`/`max`/`curve` plus a display `unit` — the pipe's whole
  *quantity* contract; presentation (`label`/`widget`) lives in a surface doc, not on the pipe
  ([ADR-0043](../adr/0043-surface-docs-decouple-presentation-from-instruments.md)). A
  defaulted pipe unfed materializes its default —
  a knob at rest, message-drivable at **`/<name>/in`** over OSC; an unfed *bare* signal pipe
  renders silence (and warns at top level, where nothing can ever feed it).
- An **output pipe is fed from an internal port**: `"main_l": { "from": "/pan.left" }`.
  Signal output pipes drive the logical master channels.
- A **signal** pipe may carry an optional logical **`channel: <int>`** binding — **honored
  only on the graph actually played at top level**: an input pipe with `channel: k` reads
  logical input channel `k` (real device audio via the input stream); an output pipe with
  `channel: k` feeds logical output channel `k`. A channel-bound pipe keeps its declared
  `default` as the unfed fallback. `channel` on a **message** pipe is a load error. An output
  pipe with `channel` omitted **broadcasts** to all logical output channels (the v1 default,
  unchanged). Logical widths derive from the played top graph: output = max bound output
  channel + 1 (floor 2), input = max bound input channel + 1 (0 if none — a patch with no
  input pipes pays nothing).
- **Nested or Voicer-hosted, `channel` is inert** — the parent feeds the pipe through the
  synthesized face like any boundary wire; a nest never reaches the hardware on its own.
  Patches never name *device* channels at all: binding logical channels to a real rig is the
  device profile's job (`play --io-map`, [docs/device-profile.md](../device-profile.md)).

```json
"interface": {
  "inputs": {
    "in":   { "type": "f32_buffer" },
    "mic":  { "type": "f32_buffer", "channel": 0 },
    "tone": { "type": "f32_buffer", "default": 4000.0, "min": 20.0, "max": 20000.0,
              "curve": "exp", "unit": "Hz" }
  },
  "outputs": {
    "main_l": { "from": "/pan.left",  "channel": 0 },
    "main_r": { "from": "/pan.right", "channel": 1 }
  }
}
```

Worked examples: `instruments/patches/space.json` (a nestable effect's typed pipes),
`instruments/mic-space.json` (a channel-bound live-input pipe feeding a nested patch),
`instruments/stereo-sub.json` + `instruments/stereo-sub.io-map.json` (three bound output
channels + the device profile that maps them), `instruments/stereo-autopan.json` (stereo
channel-pinned outputs).

A document may also carry a top-level `resources` table (logical id → source path) that
resource-bearing nodes reference by a `sample` field
([ADR-0016](../adr/0016-sample-player-and-resource-store.md)). Resolving + decoding those needs a
`ResourceResolver`, so use `format::load_instrument(json, registry, resolver)` — it returns the
`Graph` plus any non-fatal `LoadWarning`s (a missing/undecodable sample degrades to silence).
`instruments/sampler.json` is the worked example; `reuben-native` supplies a filesystem WAV resolver.
A source path resolves **relative to the document that names it** (a nested patch's own resources
live next to *it*, not next to the top-level instrument), falling back to a configurable library
root (`reuben --instrument-root <DIR>` or `REUBEN_INSTRUMENT_ROOT`); the resolver canonicalizes
identity, so `a.json` and `./a.json` are one cycle-guard/dedup key. For embedded hosts and tests,
core's in-memory `MemoryResolver` serves patches and samples by exact key with no filesystem.
A document may declare a `format_version` ([ADR-0036](../adr/0036-instrument-library-and-format-versioning.md));
absent means 1, and a version newer than the engine understands refuses to load. The current
version is **3** — [ADR-0043](../adr/0043-surface-docs-decouple-presentation-from-instruments.md)'s
presentation strip, the second breaking bump after ADR-0038's v2 interface-pipe direction
flip. Old documents keep loading forever, migrated at parse: v1's target-form `interface`
entries flip to pipes + consumer wire-refs (deriving each pipe's type/range/default from the
old target port; the anonymous `outputs` array becomes named `interface.outputs` entries), and
a leftover per-node `control` block or pipe `label`/`widget` — v2's retired presentation — is
**ignored with a `LoadWarning` naming it** (`DeprecatedControlBlock` /
`DeprecatedPipePresentation`): never fatal, never silent, and sound is unaffected (the engine
never read them; re-saving strips them). Migrated-vs-native renders are **bit-identical**
(asserted in `crates/reuben-core/tests/format_v2.rs` and `format_v3.rs`). Save writes v3 — a
migrated document never saves back under its old number. To **save**, serialize
the `InstrumentDoc` (nested references survive); `InstrumentDoc::from_graph` is the explicit
flatten/export path — a built graph's spliced subpatches appear as their inlined nodes.

A Voicer node references a **voice sub-patch** the same way, by a **`voice`** field naming a standalone
instrument JSON ([ADR-0032](../adr/0032-voicer-hosts-voice-subpatches.md)); the loader resolves it
(nested `sample` resources resolve recursively), builds it `voices` times, and binds the graphs. A
voice patch declares its **`interface`** like any graph (pipes, ADR-0038): input pipes
(`freq`/`gate`) its internal nodes consume, output pipes (`audio`/`active`) fed from internal
ports — so the host Voicer can drive and tap it through the boundary. Hosted this way, any
`channel` binding on a voice's pipes is inert, exactly as for a nested subpatch.
(`interface` is real wiring the engine type-checks — the contract a surface doc binds to,
never surface metadata itself.) See `instruments/default.json` + `instruments/voices/default-voice.json`.

### Nesting: a `subpatch` node inlined at build ([ADR-0034](../adr/0034-instrument-nesting.md))

A **`subpatch`** node references a nested instrument the same way, by a **`patch`** field naming an
instrument JSON in `resources`. At build the child is resolved recursively and **inlined**: its
nodes splice into the parent under the node's address prefix (child `/filter` inside `/space`
becomes `/space/filter` — still OSC-reachable; a post-prefix collision is a fatal
`DuplicateAddress`), every parent wire onto a boundary port is rewired straight to the inner
target, and the `subpatch` node **dissolves** — nesting costs nothing at runtime. Two uses of one
patch get disjoint prefixes, so per-reuse state isolation is automatic. Cyclic references are a
fatal `CyclicResource`; availability problems (missing id, unreadable source) degrade to a
`LoadWarning` (the node goes *dark* — references to it drop with warnings); a
resolved-but-malformed child document is fatal.

The node's ports are the child's **`interface` names** — a synthesized **boundary face**, one port
per name, each carrying the **entry's declared `Arg` type** (ADR-0038 §2, amending ADR-0034 §4's
inherit-from-the-inner-port rule: a pipe points at no inner port, so there is nothing to inherit
from and the entry declares its type itself; the ordinary pass-2 wire check covers boundary wires,
and errors speak in boundary terms — the subpatch address and external name). Wire or set literals
on those names exactly as on operator ports: `"tone": 2500` validates against the pipe's declared
type and range. Inlined this way, a child pipe's `channel` binding is **inert** (ADR-0038 §3): the
host's wiring feeds the pipe through the face; an unwired nested pipe falls back to its declared
default (silence for a bare signal pipe) with a `LoadWarning` — a nest never reaches the hardware
on its own.

A pipe entry carries its **quantity contract** alongside the declared type — for numeric
pipes the engine-enforced `default`/`min`/`max`/`curve` plus a display `unit` (ADR-0038 §2 as
amended by [ADR-0043](../adr/0043-surface-docs-decouple-presentation-from-instruments.md):
the entry owns this metadata outright, and `unit`/`curve` describe the *quantity*, so every
surface of the instrument inherits them). Presentation — `label`, `widget`, grouping, order —
lives apart in a **surface doc** (`surfaces/<name>.json`, schema
`surfaces/surface.schema.json`) that binds pipes by name; the `control-surface` skill authors
it, and the web player and TouchOSC emitter render from it. The declared `type` is what flows
(see `instruments/patches/space.json`):

```json
"interface": {
  "inputs": {
    "in":   { "type": "f32_buffer" },
    "tone": { "type": "f32_buffer", "default": 4000.0, "min": 20.0, "max": 20000.0,
              "curve": "exp", "unit": "Hz" }
  },
  "outputs": { "out": { "from": "/verb.audio" } }
}
```

`reuben describe <patch.json>` prints the boundary a host wires against — each pipe with its
declared type, range, default, and unit.
`instruments/patches/space.json` (nestable effect) + `instruments/nested-space.json` (host) are
the worked pair; `instruments/mic-space.json` nests the same effect behind a live-input pipe.

The per-node **`control`** block ([ADR-0018](../adr/0018-control-surface-generation.md)) is
**retired** ([ADR-0043](../adr/0043-surface-docs-decouple-presentation-from-instruments.md)):
a v2 document (or a v3 one still carrying leftovers) parses, but the block is dropped with a
`LoadWarning::DeprecatedControlBlock` — the engine never read it, so sound is unchanged, and
re-saving strips it. Player-facing controls are **interface input pipes** now; their
presentation lives in a surface doc read by the
[`control-surface` skill](../../.claude/skills/control-surface/SKILL.md) and the web player
(`instruments/good-button.json` + `surfaces/good-button.json` are the worked pair).

## "Audio vs control" is boundary metadata, not a type

Collapsing audio, CV, and control into one `f32_buffer` Signal means the engine treats every
`f32_buffer` alike. The authoring *intent* — "this is an audio/CV cable" vs "this is a control knob" —
that the surface resolvers and patcher care about lives at the **graph boundary** (a knob is an
interface input pipe with a declared range and default; a surface doc binds it to a widget),
never as a runtime type.

## Addressing

Every node has an OSC **address**, derived from graph structure by default. A Message targets a
node by address prefix and an **input port by name** — always addressed explicitly as
`/<node>/<input>` (ADR-0030 routes by port name; there is no whole-node sugar). An `F32` control
input takes a scalar (`/filt/cutoff 1500`), an enum input a symbol (`/filt/mode "Hp"`), a `Note`
input its args (`/voicer/notes [69.0, 1.0]`). Full wildcard dispatch (`/drums/*/decay`) is designed
but not built — today a Message targets at most one node
([ADR-0005](../adr/0005-osc-namespace-and-wildcards.md)).

## Invariants you must not break

- **Determinism** — output is bit-identical regardless of executor or thread interleaving
  ([ADR-0001](../adr/0001-unified-block-graph-execution.md)). No wall-clock, no RNG without
  a seeded, plan-owned source. **Live audio input is the one sanctioned nondeterministic
  boundary** ([ADR-0038](../adr/0038-interface-pipes-and-the-device-layer.md) §10, the same
  category as OSC-in): a patch with no input pipes gains no new nondeterminism, and offline
  render / `OpDriver` injects known buffers into input pipes, so injected-input renders stay
  bit-reproducible.
- <a id="rt-safe-render"></a>**RT-safe Render** — `render_block` is allocation-free after
  warmup, asserted by `crates/reuben-core/tests/rt_safe.rs`. Code that runs on the audio
  render thread(s) — the **hot** path — must not allocate, lock, or block, and must not
  panic. All scratch is preallocated and reused (including the materialize buffers for
  `Value → Signal` edges and the Voicer's per-voice sub-`Plan` arenas); routed events are zero-copy.
  - **The hot/cold boundary** is the audio render thread, not a file or type. **Hot** = any
    code reachable from a `fn process` body (plus the per-block render path —
    `render_block`/`render_into`/`process_node` — and the message drain/route that runs on
    the audio thread). **Cold** = everything else: `descriptor()`/`operator_contract!`,
    `new`/`Default`/`spawn`/`bind_resources`, `RenderContext` preallocation, and the whole
    Coordinator region (Instantiate, Swap-construction, (de)serialization, reclaim) plus the
    patcher/schema/CLI. The line cuts *through* a single file — `spawn` allocates by design
    inches from an alloc-free `process`. Judge each by which thread runs it.
  - **Hot-path totality** — stay panic-free with the codebase's own idioms (`map_or`,
    `unwrap_or`, `.clamp()`); a panic in the audio callback unwinds across the cpal FFI
    boundary. `debug_assert!` is fine (it vanishes in release); plain in-bounds indexing
    (`buf[i]` for `i < n`) is fine. `unsafe` on the hot path is a last resort that requires
    a committed benchmark ([ADR-0019](../adr/0019-performance-benchmarking.md)) proving it.
- **OSC-only core** — the core speaks only OSC-shaped Messages. MIDI, Ableton Link, tempo
  sync, etc. are removable boundary adapters that convert to/from OSC in the native layer
  ([ADR-0007](../adr/0007-osc-only-core.md)).
- **Single-writer boundary** — the Coordinator is the only writer of graph structure;
  Render only ever reads an immutable Plan
  ([ADR-0012](../adr/0012-boundary-and-threading.md)).

## ADR index

The decisions and reasoning behind all of the above live in [docs/adr/](../adr/) — start
there when a contract's *why* is unclear.
[ADR-0030](../adr/0030-osc-as-all-data-one-message-type.md) is the one-`Message`/`Arg` data model
this doc is built on (superseding the ADR-0028 shape model).
