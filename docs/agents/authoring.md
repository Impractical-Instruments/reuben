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

The read/write surface is **two return-type-dispatched verbs** ([ADR-0031](../adr/0031-float-resolves-to-value-or-signal-by-wiring.md)):
`io.input::<T>(port)` and `io.output::<T>(port)`. The payload type `T` selects the form and the
return shape — there is no separate `signal`/`last`/`stream` family.

| `Arg` type (form) | what it is | read view (input) / write view (output) |
|---|---|---|
| **`f32_buffer`** (a *Signal*) | dense per-sample audio / CV / control — the one buffer payload | `io.input::<&[f32]>(IN) -> &[f32]` (+ `io.varying(IN)`) · out: `io.output::<&mut [f32]>(OUT) -> &mut [f32]` |
| **`f32`** (a held *Value*) | a number — freq, cutoff, amp, a contour; owns a default, latched and read once per (sub)block | `io.input::<f32>(IN) -> Option<f32>` · out: `io.output::<f32>(OUT) -> MsgWriter` (`.set(frame, v)`) |
| **enum** (a *vocab* type, a Value) | a named discrete choice — `FilterMode`, `Waveform` | `io.input::<FilterMode>(IN).unwrap_or_default()` — a real Rust enum, not an index |
| **`Harmony`** (vocab struct, a Value) | the tonal-context struct: `root`/`scale`/`chord` + resolvers `hz()`/`snap()`/`chord_tone()` | `io.input::<Harmony>(IN) -> Option<Harmony>` · out: `io.output::<Harmony>(OUT) -> MsgWriter` (`.set(frame, h)`) |
| **`Note`** (vocab struct, an Event) | a pitch/velocity event | `io.input::<Note>(IN)` → `EventStream<Note>` of `Stamped<Note>` (`.frame`, `.payload`) · out: `io.output::<Note>(OUT) -> EventWriter` (`.emit(frame, note)`) |

A port's **form** is one of three — **Signal** (`f32_buffer`), **Value** (`f32`/enum/`Harmony`, a
held latch read once per slice), **Event** (`Note`, a sparse frame-stamped stream) — and follows from
the declared `Arg` type (`PortKind` in `plan.rs`). Reading older code: **Signal** = `f32_buffer`;
**param** = an `f32` Value or held enum; **Context** = `Harmony`; **Message events** = `Note` (the
ADR-0028 `shape`/temporality axis is gone). A runtime integer is a rounded `f32` or an enum; `I32` is
an OSC primitive `Arg`, but no operator declares an `Int` port.

### Form is declared, not inferred: `f32` is a held Value, `f32_buffer` is a Signal

The author picks a numeric port's form by which keyword it declares
([ADR-0031](../adr/0031-float-resolves-to-value-or-signal-by-wiring.md)):

- **`f32` → a held Value.** A latched scalar read once per block-slice with `io.input::<f32>(IN)`. The
  engine block-slices at each change frame, so the read is sample-accurate without a buffer. With a
  `meta` block (`f32 { range, default, .. }`) it seeds its latch from override-or-default; unwired it
  reads the default. No buffer allocated.
- **`f32_buffer` → a Signal.** A dense per-sample buffer read with `io.input::<&[f32]>(IN)` — audio, CV,
  or any *swept* control (a filter cutoff an LFO modulates). An optional `meta` default lets an
  unwired/knob-set port materialize a constant buffer, so the read is a real buffer either way.

A cheap **`varying: bool`** rides alongside a Signal read (`io.varying(IN)`): `false` when a
materialized input held unchanged this block, `true` when dense or changed. A const-folding op (a
filter recomputing biquad coefficients only when `cutoff` moves) opts in; a naive op ignores it and
reads `io.input::<&[f32]>(IN)[i]` — always correct.

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
  through the `Io` view, by `Arg` type (ADR-0030):
  - `io.input::<&[f32]>(IN) -> &[f32]` — read a **`f32_buffer`** (Signal) input per sample, or the
    materialized buffer of a Value source wired into it. `io.varying(IN)` is the change hint.
    `io.output::<&mut [f32]>(OUT) -> &mut [f32]` writes a `f32_buffer` output. Each buffer is exactly
    `io.frames()` long.
  - `io.input::<f32>(IN) -> Option<f32>` — read a held **`f32`** Value (the block-rate scalar view, a
    clock's `tempo`, a gate edge). `io.input::<E>(IN) -> Option<E>` reads an **enum** input as its
    real *vocab* type, constant for the (sub)block: `io.input::<Waveform>(IN_WAVEFORM).unwrap_or_default()`.
    No `enum_index`/`from_index` on the hot path. `io.output::<f32>(OUT) -> MsgWriter` writes a held
    Value: `.set(frame, v)` is deduped (an unchanged value emits nothing) + last-write-wins per frame.
  - `io.input::<Note>(IN)` — read **`Note`** events (Voicer, sequencer): a zero-copy `EventStream<Note>`
    iterator of `Stamped<Note>` (`.frame` segment-relative, `.payload` the decoded `Note`).
    `io.output::<Note>(OUT) -> EventWriter` writes events: `.emit(frame, payload)` is **append-only**
    (no dedup, no last-write-wins — a chord's tones at one frame all survive). Internal wires are
    **addressless** — routed by connection, not name ([ADR-0014](../adr/0014-internal-message-graph.md),
    ADR-0031 step 7). See `sequencer.rs` / `snap.rs`.
  - `io.input::<Harmony>(IN) -> Option<Harmony>` — read the latched tonal **`Harmony`** (key/scale/chord
    + resolver `hz`/`snap`/`chord_tone`), constant for the (sub)block, default C-major/12-TET when
    unwired (`.unwrap_or_default()`). A `harmony` Operator writes the other side via
    `io.output::<Harmony>(OUT_HARMONY) -> MsgWriter` (a Harmony is a held Value, dedup+LWW is correct).
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
the `IN_/OUT_/P_` index consts **and** an inherent `fn contract() -> Descriptor` from the same
tokens, so consts and descriptor can't drift. An `enum` port **references a shared *vocab* enum** by
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

// An operator with an instantiate-time Constant declares it with `constant:` (the Voicer):
crate::operator_contract!(Voicer {
    inputs:  { notes: note, harmony: harmony },
    outputs: { audio: f32_buffer },
    params:  { voices: { 1.0..=32.0, default 8.0, "", lin } },
    resources: { voice },                                      // an instrument-resource slot (the voice sub-patch)
    constant: voices,                                          // instantiate-time config — rebuilds the graph if changed
});

impl Operator for Filter {
    fn descriptor() -> Descriptor { Self::contract() }   // one-liner delegate (ADR-0025)
    fn process(&mut self, io: &mut Io) {
        let mode = io.input::<FilterMode>(IN_MODE).unwrap_or_default();  // a real Rust enum
        // per sample: let x = io.input::<&[f32]>(IN_AUDIO); let cut = io.input::<&[f32]>(IN_CUTOFF);  (`varying` lets it const-fold)
        // one buffer loop over io.frames(), writing io.output::<&mut [f32]>(OUT_AUDIO) ...
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
- **`name: enum(VocabType)`** — an enum (Value) input naming its shared *vocab* type (`Port::enumerated`
  off `VocabType::enum_meta`); the type's `#[default]` variant is the default.
- **`name: note`** / **`name: harmony`** — `Note` (Event) / `Harmony` (Value) ports (`Port::note` /
  `Port::harmony`).
- **`name: arg`** — a **type-agnostic pass-through** (issue #141): carries *any* `Arg` as a raw
  Event stream (`Port::arg`), read via `io.input::<&Arg>` and re-emitted via `io.output::<Arg>`.
  **Input-only**, and only for a **pure carrier** — an operator that treats the payload as opaque
  (forward, buffer, drop) and never interprets it; the wired *source* port is the type authority.
  Legality is capability-keyed: any Event or Value source whose type has an **external OSC form**
  wires in (primitives, vocab enums, `Note`'s flat form); a `Harmony` source (no OSC form —
  converters are issue #146) and a Signal source are rejected at load/plan — audio never crosses
  the boundary. Inbound is asymmetric: external OSC addressed at an `arg` port crosses only as a
  **single numeric atom** (multi-arg lists and strings drop — so the flat 2-arg Note form the sink
  *sends* does not round-trip back in through an `arg` port; a typed `note` port still decodes
  it). Today the form of `osc_out.in`, the outbound OSC sink.
- **`params: { name: { ..range } }` + `constant: name`** — declares one param an instantiate-time
  `Constant` (the Voicer's `voices`); the loader routes it to the patch's `config` block. At most one
  `Constant` per operator.

Other notes:

- An operator with no explicit `type_name:` takes the snake_case of its struct name; pass
  `type_name: "sample"` when they diverge (e.g. `SamplePlayer`).
- **Ports are referenced by name** in the JSON format, not by index — names are the stable
  contract the rig builder wires against. The macro computes the ordinals.
- **Exceptions:** `output` is the lone operator that still hand-writes `descriptor()`, where the
  macro DSL can't express its contract. Everything else delegates to the macro via
  `Self::contract()` — including `m2s` / `oscillator` (now macro-expressible: a shared-vocab enum
  default falls out of the type's `#[default]`). One operator per module since
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
   the engine actually seeds and steps a node. Address ports by the generated `IN_*` / `OUT_*` consts:
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

An Instrument is plain JSON data ([ADR-0028](../adr/0028-one-input-shape.md)): `nodes` (operator
`type` + `address`, plus an `inputs` map, an optional `config` block, and optional `doc`) and
master `outputs`. There is **no top-level `connections` array** and **no per-node `params` map** —
both fold into `inputs`.

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

A document may also carry a top-level `resources` table (logical id → source path) that
resource-bearing nodes reference by a `sample` field
([ADR-0016](../adr/0016-sample-player-and-resource-store.md)). Resolving + decoding those needs a
`ResourceResolver`, so use `format::load_instrument(json, registry, resolver)` — it returns the
`Graph` plus any non-fatal `LoadWarning`s (a missing/undecodable sample degrades to silence).
`instruments/sampler.json` is the worked example; `reuben-native` supplies a filesystem WAV resolver.

A Voicer node references a **voice sub-patch** the same way, by a **`voice`** field naming a standalone
instrument JSON ([ADR-0032](../adr/0032-voicer-hosts-voice-subpatches.md)); the loader resolves it
(nested `sample` resources resolve recursively), builds it `voices` times, and binds the graphs. A
voice patch declares a top-level **`interface { inputs, outputs }`** block — the engine-honored I/O
boundary mapping external names (`freq`/`gate`/`audio`/`active`) to internal `(node, port)` pairs —
so the host Voicer can drive and tap it. (`interface` is real wiring the engine type-checks, distinct
from the engine-ignored `control` block.) See `instruments/default.json` + `instruments/voices/default-voice.json`.

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
per name, each carrying the **inner port's `Arg` type verbatim** (§4: the type is inherited and
*never* overridable; the ordinary pass-2 wire check covers boundary wires, and errors speak in
boundary terms — the subpatch address and external name). Wire or set literals on those names
exactly as on operator ports: `"tone": 2500` validates against the inner port the interface names.

An `interface` entry is a bare `/node.port` string, or an **object form** carrying
**presentational-metadata overrides** (ADR-0034 §4) — `label`, `unit`, `widget`, `min`/`max` —
inherited from the inner port and overridable per-field. Overrides decorate how a boundary control
*presents* (introspection, control-surface generation); they never change what type flows, and
there is deliberately no field to express a type override (`deny_unknown_fields` rejects one):

```json
"interface": {
  "inputs": {
    "in":   "/filter.audio",
    "tone": { "target": "/filter.cutoff", "label": "Tone", "widget": "knob", "min": 200, "max": 8000 }
  },
  "outputs": { "out": "/verb.audio" }
}
```

`reuben describe <patch.json>` prints the boundary a host wires against — each `interface` port
with metadata inherited from the inner port (the *effective* default: a child literal like
`"mix": 0.35` beats the descriptor default) and the entry's overrides applied.
`instruments/patches/space.json` (nestable effect) + `instruments/nested-space.json` (host) are
the worked pair.

A node may also carry an optional **`control`** block
([ADR-0018](../adr/0018-control-surface-generation.md)) — surface metadata marking it
player-facing: a `label` (required) plus optional `unit`/`widget`/range, a `param` (to bind a
specific input instead of the node address), or `widget: "note-toggle"` with a `note`/`port`.
It is **opaque to the engine** — round-trips through load/save, never read at runtime; the
[`control-surface` skill](../../.claude/skills/control-surface/SKILL.md) reads it to generate a
TouchOSC surface. `instruments/good-button.json` is the worked example.

## "Audio vs control" is tooling metadata, not a type

Collapsing audio, CV, and control into one `f32_buffer` Signal means the engine treats every
`f32_buffer` alike. The authoring *intent* — "this is an audio/CV cable" vs "this is a control knob" —
that the control-surface generator and patcher care about survives as **optional tooling metadata**
(next to `control`), never as a runtime type.

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
  a seeded, plan-owned source.
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
