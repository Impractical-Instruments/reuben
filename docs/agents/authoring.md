# Authoring: Operators, Instruments, Rigs

The grounding doc for building reuben — the concrete code contract behind the conceptual
narrative in [ARCHITECTURE.md](../ARCHITECTURE.md). Capitalized terms (Operator, Lane,
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
piece of typed data — its **`Arg`** type, drawn from one closed, central enum. How densely the
value arrives, how it is read, and whether it can be held all **follow from the `Arg` type plus
the read verb**; none of it is a separate thing the author declares. Outputs carry an `Arg` the
same way. (The ADR-0028 **`shape`** axis is **retired** — the axis is now the port's `Arg` type.)

| `Arg` type | what it is | read view (input) / write view (output) |
|---|---|---|
| **`Buffer`** (a *Signal*) | dense per-sample audio / CV / control — the one buffer payload | `io.signal(IN) -> &[f32]` (+ `io.varying(IN)`) · out: `io.signal_mut(OUT) -> &mut [f32]` |
| **`F32` control** (macro `float`) | a number — freq, cutoff, amp, a contour; owns a default, ZOH-materialized into a buffer | per-sample `io.signal(IN)` (materialized) · held scalar `io.last::<f32>(IN) -> Option<f32>` · out: `io.signal_mut(OUT)` |
| **enum** (a *vocab* type) | a named discrete choice — `FilterMode`, `Waveform` | `io.last::<FilterMode>(IN).unwrap_or_default()` — a real Rust enum, not an index |
| **`Harmony`** (vocab struct) | the tonal-context struct: `root`/`scale`/`chord` + resolvers `hz()`/`snap()`/`chord_tone()` | `io.last::<Harmony>(IN) -> Option<Harmony>` · out: `io.emit(OUT, "harmony", h, frame)` |
| **`Note`** (vocab struct) | a pitch/velocity event | `io.stream::<Note>(IN)` → `Stamped<Note>` (`.frame`, `.payload`) · out: `io.emit(OUT, "notes", Note::new(..), frame)` |

There is **no separate "carrier"** and no temporality axis — the old `Signal`/`Message`/`Context`
carrier (`PortKind`) is gone, and so is the ADR-0028 `shape`. The mapping for anyone reading older
code: **Signal** = a `Buffer` Arg; **param** = an `F32` control read as a held scalar
(`io.last::<f32>`), or a held enum; **Context** = `Harmony`; **Message events** = `Note`. A runtime
integer is a rounded `F32` (a modulatable step/divisor) or an enum (a bounded set); `I32` exists as
an OSC-primitive `Arg`, but no operator declares an `Int` port. The reads unify to a small verb set:
`io.signal` / `io.last::<T>` / `io.stream::<T>` to read, `io.signal_mut` / `io.emit` to write.

### Density is the engine's job; an `F32` control is always a buffer underneath

For an `F32` control, *dense vs held* is a performance detail the engine decides from the wired
source, never something the author declares:

- wired to a dense `Buffer` producer (audio, a contour) → the real buffer, passed through;
- fed by a literal / sparse changes / unwired → a scratch buffer **ZOH-materialized** from the
  latched value, with a mid-block change **written into the buffer at its frame** (so
  sample-accuracy is automatic, one `process()` call, no re-slicing). Held-unchanged values are
  **cached** — refilled only on change — so steady-state cost is ~nil. This `F32 → Buffer`
  materialize is the **one** implicit bridge in the engine (ADR-0030).

A cheap **`varying: bool`** rides alongside (`io.varying(IN)`): `false` when a materialized input
held its value unchanged this block, `true` when dense or changed. A const-folding op (e.g. a filter
recomputing biquad coefficients only when `cutoff` moves) opts into it; a naive op ignores it and
reads `io.signal(IN)[i]` — always correct.

### Two read views on an `F32` control, chosen by the processing model

An `F32` control exposes two read views over the same latched state — a **static** choice intrinsic
to the operator, never conditional on what's wired:

- **per-sample DSP** (osc, filter, `mul`, `power`, envelope) → `io.signal(IN)` (the materialized
  buffer) + the `varying` hint;
- **block-rate / scalar** (a clock reading tempo, a sequencer reading `length`) →
  `io.last::<f32>(IN)`, the held value without looping a buffer.

A filter always calls `io.signal`; a sequencer reads `length` with `io.last::<f32>`. Outputs mirror
this: per-sample producers write `io.signal_mut(OUT)`.

### Cross-type use is always an explicit converter

A producer and consumer never need matching density — each is just an `Arg` type, the engine
bridges. The one **illegal** wiring is an `Arg`-type mismatch (`"audio": "Hp"`, or a `Buffer` wired
into a `Note` input) — a `TypeMismatch` load error (it compares the two
ports' `PortType`s), the successor to `PortKindMismatch`. The sole implicit coercion is the
`F32 → Buffer` ZOH materialize above; every other crossing needs an operator: `F32 → enum` is a
quantizer; `F32 → Note` is a threshold/trigger; `slew`/`glide` are `F32 → F32` shapers (the `m2s`
gap-filling modes). `m2s`'s old `Snap` (plain step) job *is* the engine's automatic materialize, so
it needs no node.

### `Constant` — instantiate-time configuration, not an `Input`

A **`Constant`** configures an operator *instance* at instantiate time and never changes on the
data path. The boundary is precise: **a value is a `Constant` iff changing it would rebuild the
graph.** The canonical (and today only) case is the Voicer's `voices` — it sets Lane count, hence
buffer allocation and topology (`LaneRule::FromParam`), so it can't be a runtime value. Constants
live in the patch's `config` block, not `inputs`.

**`Arg` type does not decide `Constant`-vs-`Input`.** `mode` (Lp/Hp/Bp) and `waveform` (Sine/Saw)
are enums, but changing them rebuilds nothing — only which coefficients run — so they are **runtime
enum inputs**, switchable live over OSC. Only genuinely topology-fixing values are `Constant`s.

## The Operator contract (`crates/reuben-core/src/operator.rs`)

Operators are authored **single-Lane** ([ADR-0010](../adr/0010-single-lane-operators.md)):
you write one mono, single-Voice stream a (sub)block at a time, and the engine fans it out
across Lanes (Voice × Channel) with per-Lane state. The trait is three methods (plus an optional
resource hook):

```rust
pub trait Operator: Send {
    /// Static self-description (ports + metadata). Drives serialization, connection
    /// checking, good-button controls, and AI grounding.
    fn descriptor() -> Descriptor where Self: Sized;

    /// Process exactly one (sub)block for one Lane. Must not allocate.
    fn process(&mut self, io: &mut Io);

    /// Fresh-state instance of the same type, for another Voice's Lane.
    fn spawn(&self) -> Box<dyn Operator>;

    /// Receive decoded resources after construction, before fan-out. Default no-op;
    /// only resource-bearing operators (the sample player) override it.
    fn bind_resources(&mut self, store: &Arc<ResourceStore>, refs: &ResolvedRefs) {}
}
```

- **`descriptor()`** — see below. The single source of an operator's ports and metadata.
- **`process(io)`** — the only realtime path. **Allocation-free.** Read inputs, write outputs
  through the `Io` view, by `Arg` type (ADR-0030):
  - `io.signal(IN) -> &[f32]` — read a **`Buffer`** input, or the materialized buffer of an
    **`F32` control**, per sample. `io.last::<f32>(IN) -> Option<f32>` reads the held scalar of an
    `F32` control (the block-rate view). `io.varying(IN)` is the change hint.
    `io.signal_mut(OUT) -> &mut [f32]` writes a `Buffer` output. Each buffer is exactly
    `io.frames()` long.
  - `io.last::<E>(IN) -> Option<E>` — read an **enum** input as its real *vocab* type, constant for
    the (sub)block (the engine slices at enum changes): `io.last::<Waveform>(IN_WAVEFORM).unwrap_or_default()`.
    No more `enum_index`/`from_index` on the hot path.
  - `io.stream::<Note>(IN)` — read **`Note`** events (Voicer, sequencer): a zero-copy iterator of
    `Stamped<Note>` (`.frame` segment-relative, `.payload` the decoded `Note`). `io.emit(OUT, addr,
    payload, frame)` emits one Message onto an output port ([ADR-0014](../adr/0014-internal-message-graph.md));
    `addr` is `&'static str` and `payload` is one `Arg` (`impl Into<Arg>`), so it allocates nothing.
    Emission is single-Lane (Lane 0 only). See `sequencer.rs` / `snap.rs`.
  - `io.last::<Harmony>(IN) -> Option<Harmony>` — read the latched tonal **`Harmony`** (key/scale/chord
    + resolver `hz`/`snap`/`chord_tone`), constant for the (sub)block, default C-major/12-TET when
    unwired (`.unwrap_or_default()`). A `harmony` Operator writes the other side by **emitting** the
    `Harmony` Arg — `io.emit(OUT_HARMONY, "harmony", h, frame)` (single-Lane) — since publishing a Harmony is
    just a Message on a Harmony port now. The Voicer and `snap.rs` read it; `harmony.rs` emits it.
    *(The struct is named `Harmony` in code (`vocab/harmony.rs`); the legacy `io.harmony`/`io.publish_harmony`
    accessors are gone, folded into `io.last`/`io.emit`. The publishing Operator's author-facing type
    is `"harmony"` (`operators/harmony.rs`).)*
  - `io.lane()` / `io.lanes()` — most operators ignore these; an *expander* like the Voicer uses
    them to emit one Voice's output per call.
- **`spawn()`** — usually `Box::new(Self::new())`. Resets per-Lane state only. A resource-bearing
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
    inputs:  { audio: buffer,                                  // a Buffer (audio/CV) input
               cutoff: float { 20.0..=20_000.0, default 1_000.0, "Hz", exp },  // materialized F32 + default
               resonance: float { 0.0..=1.0, default 0.2, "", lin },
               mode: enum(FilterMode) },                       // a live-switchable enum, shared vocab type
    outputs: { audio: buffer },
    // params: { voices: { 1.0..=16.0, default 8.0, "", lin } }, lanes: from_param(voices),  // a Constant — see the Voicer
    lanes: inherit,                                            // default; or from_param(<param>) for an expander
});

impl Operator for Filter {
    fn descriptor() -> Descriptor { Self::contract() }   // one-liner delegate (ADR-0025)
    fn process(&mut self, io: &mut Io) {
        let mode = io.last::<FilterMode>(IN_MODE).unwrap_or_default();  // a real Rust enum
        // per sample: let x = io.signal(IN_AUDIO); let cut = io.signal(IN_CUTOFF);  (`varying` lets it const-fold)
        // one buffer loop over io.frames(), writing io.signal_mut(OUT_AUDIO) ...
    }
    // spawn ...
}
```

`Arg`-type forms in the macro (each emits the matching `Port::*` constructor):

- **`name: buffer`** — a `Buffer` (audio/CV) port with no settable default, e.g. a passthrough
  `audio` in or any per-sample output (`Port::buffer`).
- **`name: float { MIN..=MAX, default D, "unit", lin|exp }`** — a **materialized `F32` control**
  input that owns its unwired default (the old "signal port + same-named param", now one
  declaration; `Port::float`). `"unit"` and the curve are each optional.
- **`name: enum(VocabType)`** — an enum input naming its shared *vocab* type (`Port::enumerated` off
  `VocabType::enum_meta`); the type's `#[default]` variant is the default.
- **`name: note`** / **`name: harmony`** — `Note` / `Harmony` ports (`Port::note` / `Port::harmony`).
- **`params: { name: { ..range } }` + `lanes: from_param(name)`** — a `Constant`. Today derived from
  `LaneRule::FromParam` (the Voicer's `voices`); the loader routes it to the patch's `config` block.

Other notes:

- An operator with no explicit `type_name:` takes the snake_case of its struct name; pass
  `type_name: "sample"` when they diverge (e.g. `SamplePlayer`).
- **Ports are referenced by name** in the JSON format, not by index — names are the stable
  contract the rig builder wires against. The macro computes the ordinals.
- **Exceptions:** `output` is the lone operator that still hand-writes `descriptor()`, where the
  macro DSL can't express its contract. Everything else delegates to the macro via
  `Self::contract()` — including `m2s` / `map` / `oscillator` (now macro-expressible: a shared-vocab
  enum default falls out of the type's `#[default]`) and the math family (`add`, `mul`, `power`,
  `differentiate`, `integrate`) — one operator per module since
  [ADR-0029](../adr/0029-math-family-dense-float-one-file-per-op.md) deleted the old `math.rs`
  multi-op module.
- **`LaneRule`** — `Inherit` (Lane count = max of input Lane counts; the default) or
  `FromParam(slot)` (this operator *expands*, producing that many Lanes; the Voicer is the
  canonical expander, sized by the `voices` `Constant`). Read once at Instantiate — it's structural.

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

A node may also carry an optional **`control`** block
([ADR-0018](../adr/0018-control-surface-generation.md)) — surface metadata marking it
player-facing: a `label` (required) plus optional `unit`/`widget`/range, a `param` (to bind a
specific input instead of the node address), or `widget: "note-toggle"` with a `note`/`port`.
It is **opaque to the engine** — round-trips through load/save, never read at runtime; the
[`control-surface` skill](../../.claude/skills/control-surface/SKILL.md) reads it to generate a
TouchOSC surface. `instruments/good-button.json` is the worked example.

## "Audio vs control" is tooling metadata, not a type

Collapsing audio, CV, and control into one `Buffer` Arg means the engine treats every `Buffer`
alike. The authoring *intent* — "this is an audio/CV cable" vs "this is a control knob" — that the
control-surface generator and patcher care about survives as **optional tooling metadata** (next to
`control`), never as a runtime type.

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
  panic. All scratch is preallocated and reused (including the materialize buffers for held
  `F32` controls); routed events are zero-copy.
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
