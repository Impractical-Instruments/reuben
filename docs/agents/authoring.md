# Authoring: Operators, Instruments, Rigs

The grounding doc for building reuben — the concrete code contract behind the conceptual
narrative in [ARCHITECTURE.md](../ARCHITECTURE.md). Capitalized terms (Operator, Lane,
Plan…) are defined in [CONTEXT.md](../../CONTEXT.md). The ADRs are the source of truth;
this doc tells you where the contract lives in code and how to extend it.

## The recursive model

One concept at every scale ([ADR-0003](../adr/0003-recursive-composition.md)): a graph of
nodes with shaped ports.

- **Operator** — the smallest unit of behavior; does one simple thing.
- **Instrument** — a named subgraph of Operators exposing boundary ports; reusable inside
  another Instrument *as if it were an Operator*.
- **Rig** — a full playable system: Instruments wired with routing.

Nesting is an authoring concept only; at runtime everything inlines into one flat graph.

## One `Input`, one axis: `shape` ([ADR-0028](../adr/0028-one-input-shape.md))

Every functional input an operator consumes is **one `Input`**, declared once, carrying one
piece of information — its **`shape`**, drawn from a closed, named set. How densely the value
arrives, how it is read, and whether it can be held all **follow from the shape**; none of it
is a separate thing the author declares. Outputs are shaped the same way.

| `shape` | what it is | delivery | read view (input) / write view (output) |
|---|---|---|---|
| **`Float`** | a number — freq, cutoff, amp, a contour, a control | a per-sample stream, materialized from a latched scalar | `io.signal(IN) -> &[f32]` (+`io.varying(IN)`) **or** `io.value(IN) -> f32` · out: `io.signal_mut(OUT) -> &mut [f32]` |
| **`Enum`** | a named discrete choice — filter `mode`, osc `waveform` | a held scalar, block-sliced on change | `io.enum_index(IN) -> usize` → `E::from_index(..)` |
| **`Harmony`** | the tonal-context struct: `root`/`scale`/`chord` + resolvers `hz()`/`snap()`/`chord_tone()` | a held struct, block-sliced on change | `io.harmony(IN) -> Harmony` · out: `io.publish_harmony(OUT, frame, h)` |
| **`Note`** | a pitch/velocity event | a sparse, frame-stamped event list | `io.events() -> &[Event]` · out: `io.emit(OUT, addr, args, frame)` |

There is **no separate "carrier"** and no temporality axis — the old `Signal`/`Message`/`Context`
carrier (`PortKind`) is gone. The mapping for anyone reading older code: **Signal** = a `Float`'s
buffer view; **param** = a `Float` read as a scalar, or a held `Enum`; **Context** = `Harmony`;
**Message events** = `Note`. `Int` is **not** a runtime shape — a runtime integer is a rounded
`Float` (a modulatable step/divisor) or an `Enum` (a bounded set). It survives only as a
`Constant` shape (below).

### Density is the engine's job; a `Float` is always a buffer underneath

For a `Float`, *dense vs held* is a performance detail the engine decides from the wired source,
never something the author declares:

- wired to a dense `Float` producer (audio, a contour) → the real buffer, passed through;
- fed by a literal / sparse changes / unwired → a scratch buffer **materialized** from the
  latched scalar, with a mid-block change **written into the buffer at its frame** (so
  sample-accuracy is automatic, one `process()` call, no re-slicing). Held-unchanged values are
  **cached** — refilled only on change — so steady-state cost is ~nil.

The buffer carries a cheap **`varying: bool`** (`io.varying(IN)`): `false` when a held value was
unchanged this block, `true` when dense or changed. A const-folding op (e.g. a filter recomputing
biquad coefficients only when `cutoff` moves) opts into it; a naive op ignores it and reads
`io.signal(IN)[i]` — always correct.

### Two read views on a `Float`, chosen by the processing model

A `Float` exposes two read views over the same state — a **static** choice intrinsic to the
operator, never conditional on what's wired:

- **per-sample DSP** (osc, filter, `mul`, `power`, envelope) → `io.signal(IN)` + the `varying` hint;
- **block-rate / scalar** (a clock reading tempo, a sample-and-hold) → `io.value(IN)`, reading the
  latched scalar without looping a buffer.

A filter always calls `io.signal`; a clock always calls `io.value`. Outputs mirror this:
per-sample producers write `io.signal_mut(OUT)`; the engine materializes a block-rate scalar
output into a held `Float` for downstream.

### Cross-shape use is always an explicit converter

A producer and consumer never need matching density — each is just a shape, the engine bridges. The
one **illegal** wiring is a **shape mismatch** (`"audio": "Hp"`, or a `Float` wired into a `Note`
input) — a `ShapeMismatch` load error, the successor to `PortKindMismatch`. Crossing a *shape*
boundary needs an operator, never implicit coercion: `Float → Enum` is a quantizer; `Float → Note`
is a threshold/trigger; `slew`/`glide` are `Float → Float` shapers (the old `m2s` smoothing modes,
now plain ops — its "make it a Signal" job is the engine's automatic materialize, so `m2s`'s `snap`
mode *is* the default and needs no node).

### `Constant` — instantiate-time configuration, not an `Input`

A **`Constant`** configures an operator *instance* at instantiate time and never changes on the
data path. The boundary is precise: **a value is a `Constant` iff changing it would rebuild the
graph.** The canonical (and today only) case is the Voicer's `voices` — it sets Lane count, hence
buffer allocation and topology (`LaneRule::FromParam`), so it can't be a runtime value. Constants
live in the patch's `config` block, not `inputs`.

**Shape does not decide `Constant`-vs-`Input`.** `mode` (Lp/Hp/Bp) and `waveform` (Sine/Saw) are
`Enum`s, but changing them rebuilds nothing — only which coefficients run — so they are **runtime
`Enum` inputs**, switchable live over OSC. Only genuinely topology-fixing values are `Constant`s.

## The Operator contract (`crates/reuben-core/src/operator.rs`)

Operators are authored **single-Lane** ([ADR-0010](../adr/0010-single-lane-operators.md)):
you write one mono, single-Voice stream a (sub)block at a time, and the engine fans it out
across Lanes (Voice × Channel) with per-Lane state. The trait is three methods (plus an optional
resource hook):

```rust
pub trait Operator: Send {
    /// Static self-description (ports + metadata). Drives serialization, shape checking,
    /// good-button controls, and AI grounding.
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
  through the `Io` view, by shape:
  - `io.signal(IN) -> &[f32]` / `io.value(IN) -> f32` — read a **`Float`** input (per-sample buffer
    or block-rate scalar). `io.varying(IN)` is the change hint. `io.signal_mut(OUT) -> &mut [f32]`
    writes a `Float` output. Each buffer is exactly `io.frames()` long.
  - `io.enum_index(IN) -> usize` — read an **`Enum`** input's current variant index, constant for
    the (sub)block (the engine slices at enum changes). Map it to the generated enum type:
    `Waveform::from_index(io.enum_index(IN_WAVEFORM)).unwrap_or_default()`.
  - `io.events() -> &[Event]` — read **`Note`** events (Voicer, sequencer): zero-copy views of
    routed Messages, address local to the node, segment-relative `frame`. `io.emit(OUT, addr,
    args, frame)` emits a `Note`/Message onto a Note output ([ADR-0014](../adr/0014-internal-message-graph.md));
    `addr` is `&'static str` so it allocates nothing. Emission is single-Lane (Lane 0 only). See
    `sequencer.rs`.
  - `io.harmony(IN) -> Harmony` — read the latched tonal **`Harmony`** (key/scale/chord + resolver
    `hz`/`snap`/`chord_tone`), constant for the (sub)block, default C-major/12-TET when unwired. A
    `context` Operator writes the other side with `io.publish_harmony(OUT, frame, h)` (single-Lane).
    The Voicer and `snap.rs` read it; `context.rs` publishes it. *(The struct is named `Harmony`
    in code (`harmony.rs`); `io.harmony`/`io.publish_harmony` are the only accessors — the legacy
    `io.context`/`io.publish_context` aliases are gone. The publishing Operator's author-facing
    type stays `"context"`.)*
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
serialization, shape checking, and AI grounding
([ADR-0004](../adr/0004-ai-authorability-first-class.md)).

You declare it **once**, in an `operator_contract!` call
([ADR-0025](../adr/0025-single-source-operator-contract.md)). The macro plants, at module scope,
the `IN_/OUT_/P_` index consts **and** an inherent `fn contract() -> Descriptor` from the same
tokens — and, for each `enum` declaration, the generated enum type (`VARIANTS` / `from_index` /
`DEFAULT_INDEX`) — so consts, descriptor, and enum types can't drift. The trait's `descriptor()`
delegates to it:

```rust
crate::operator_contract!(Filter {
    inputs:  { audio: float,                                   // bare float: a Float buffer in
               cutoff: float { 20..=20_000, default 1000, "Hz", exp },  // materialized Float + default
               resonance: float { 0.0..=1.0, default 0.2, "", lin },
               mode: enum { Lp, Hp, Bp } },                    // a live-switchable Enum input
    outputs: { audio: float },
    // config: { voices: int { 1..=16, default 8 } },          // a Constant — see the Voicer
    lanes: inherit,                                            // or from_param(<param>) for an expander
});

impl Operator for Filter {
    fn descriptor() -> Descriptor { Self::contract() }   // one-liner delegate (ADR-0025)
    fn process(&mut self, io: &mut Io) {
        let audio = io.signal(IN_AUDIO);
        let cut   = io.signal(IN_CUTOFF);                 // buffer view; `varying` lets it const-fold
        let mode  = Mode::from_index(io.enum_index(IN_MODE)).unwrap_or_default();
        // one buffer loop over io.frames() ...
    }
    // spawn ...
}
```

Port-shape forms in the macro:

- **`name: float`** — a bare `Float` (an audio/CV buffer with no settable default, e.g. a
  passthrough `audio` in or any output).
- **`name: float { MIN..=MAX, default D, "unit", lin|exp }`** — a **materialized** `Float` input
  that owns its unwired default (the old "signal port + same-named param", now one declaration).
- **`name: enum { A, B, C }`** — an `Enum` input; the first variant is the default. Generates the
  enum type alongside.
- **`name: message`** / **`name: context`** — `Note` / `Harmony` ports (the keyword names predate
  the shape vocabulary; they set `Shape::Note` / `Shape::Harmony`).
- **`config: { name: int { ..range } | enum { .. } }`** — a `Constant`. Today derived from
  `LaneRule::FromParam` (the Voicer's `voices`); the loader routes it to the patch's `config` block.

Other notes:

- An operator with no explicit `type_name:` takes the snake_case of its struct name; pass
  `type_name: "sample"` when they diverge (e.g. `SamplePlayer`).
- **Ports are referenced by name** in the JSON format, not by index — names are the stable
  contract the rig builder wires against. The macro computes the ordinals.
- **Exceptions:** `math.rs` (five operators in one module) and `context.rs` / `sequencer.rs`
  (param banks built by a loop) keep a hand-written `descriptor()` — the macro is for the
  static-contract, one-operator-per-module common case.
- **`LaneRule`** — `Inherit` (Lane count = max of input Lane counts; the default) or
  `FromParam(slot)` (this operator *expands*, producing that many Lanes; the Voicer is the
  canonical expander, sized by the `voices` `Constant`). Read once at Instantiate — it's structural.

### Enum over the wire: symbol primary, index fallback

An `Enum` input is addressed **by symbol** — its variant name (`/filt/mode "Hp"`, `"mode": "Hp"`):
the human-legible, refactor-stable form, and what an OSC string carries. A bare **integer index**
(`/filt/mode 1`) is accepted as a **fallback**, in range. Resolution lives in one place —
`EnumMeta::resolve` — single-sourced with the generated enum type's `VARIANTS`/`from_index`. An
unknown symbol or out-of-range index is an **error** — it never silently snaps to the default.

## Adding an Operator

1. **Create** `crates/reuben-core/src/operators/<name>.rs` — a struct + `impl Operator`.
   Declare the contract once with `crate::operator_contract!(..)` and delegate
   `fn descriptor() -> Descriptor { Self::contract() }`. Follow `lfo.rs` (simplest source op),
   `filter.rs` (Float inputs with defaults + an Enum), or `delay.rs` (input + state) as templates.
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
5. **Test** in the operator module, test-first. At minimum cover: output correctness,
   phase/state continuity across back-to-back blocks (one whole block == two half-blocks
   sharing the instance), and that a `spawn()`ed copy starts fresh. See `lfo.rs` tests.

Embedders can add their own types without touching the core via `Registry::register` — the
seam for the "agents author new Operators in Rust" goal ([ADR-0004](../adr/0004-ai-authorability-first-class.md)).

## The Instrument format (`crates/reuben-core/src/format.rs`)

An Instrument is plain JSON data ([ADR-0028](../adr/0028-one-input-shape.md)): `nodes` (operator
`type` + `address`, plus an `inputs` map, an optional `config` block, and optional `doc`) and
master `outputs`. There is **no top-level `connections` array** and **no per-node `params` map** —
both fold into `inputs`.

Each entry in a node's **`inputs`** map is one of:

- a **literal** — `"resonance": 0.4` (a `Float` default) or `"mode": "Hp"` (an `Enum` by symbol);
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
edges (checking shapes), and returns a `Graph`. Loading is an authoring step — portable core,
never the audio thread. Errors are specific: `UnknownInput`, `BadInputValue`, `ShapeMismatch`,
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

Collapsing audio, CV, and control into one `Float` shape means the engine treats every `Float`
alike. The authoring *intent* — "this is an audio/CV cable" vs "this is a control knob" — that the
control-surface generator and patcher care about survives as **optional tooling metadata** (next to
`control`), never as a runtime type.

## Addressing

Every node has an OSC **address**, derived from graph structure by default. A Message targets a
node by address prefix; the local remainder becomes the `Event` address (e.g. `/voicer/note` under
node `/voicer` arrives as event `note`). A `Float` input takes a scalar set at `/<node>/<input>`
(e.g. `/filt/cutoff 1500`); an `Enum` input takes a symbol (`/filt/mode "Hp"`). Full wildcard
dispatch (`/drums/*/decay`) is designed but not built — today a Message targets at most one node
([ADR-0005](../adr/0005-osc-namespace-and-wildcards.md)).

## Invariants you must not break

- **Determinism** — output is bit-identical regardless of executor or thread interleaving
  ([ADR-0001](../adr/0001-unified-block-graph-execution.md)). No wall-clock, no RNG without
  a seeded, plan-owned source.
- <a id="rt-safe-render"></a>**RT-safe Render** — `render_block` is allocation-free after
  warmup, asserted by `crates/reuben-core/tests/rt_safe.rs`. Code that runs on the audio
  render thread(s) — the **hot** path — must not allocate, lock, or block, and must not
  panic. All scratch is preallocated and reused (including the materialize buffers for held
  `Float`s); routed events are zero-copy.
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
there when a contract's *why* is unclear. [ADR-0028](../adr/0028-one-input-shape.md) is the
shape model this doc is built on.
