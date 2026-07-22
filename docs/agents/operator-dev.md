# Operator development

The builder doc — how to author a new **Operator** in Rust: the `Operator` trait, the
`operator_contract!` descriptor macro, registration, the add-an-operator steps, and `OpDriver`
testing. Its counterpart is the [instrument-authoring guide](authoring.md), which owns the
material this doc builds on and does not restate: the `Arg`/form **type system and wiring
rules** ([authoring.md#type-system](authoring.md#type-system)), the instrument JSON format,
and the engine-wide invariants. Capitalized terms (Operator, Voice, Plan…) are defined in the
[rules corpus glossary](../rules/README.md#glossary), the source of truth.

The [`create-operator` skill](../../.claude/skills/create-operator/SKILL.md) drives this
doc's workflow end to end; the
[`rust-hot-path-review` skill](../../.claude/skills/rust-hot-path-review/SKILL.md) is its
review mirror over a finished diff.

## The Operator contract (`crates/reuben-core/src/operator.rs`)

Operators are authored **single-Voice** ([composition-operators](../rules/composition-operators.md)):
you write one mono, single-Voice stream a (sub)block at a time. Polyphony is not a per-operator
fan-out — the **Voicer** hosts N voice sub-patches and sums them,
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

Two optional lifecycle hooks support the Voicer,
both default no-ops: `bind_voices(Vec<Graph>)` receives the N built voice sub-patches at load, and
`on_instantiate(&AudioConfig) -> Result<(), PlanError>` runs per node from `Plan::instantiate` (the
one place with the audio config) so the Voicer can build each voice's sub-`Plan` + arena off the hot
path.

A third default-no-op hook, `on_transplant(&mut self)`, runs on a **survivor** box just after it is
transplanted across a hot-swap ([execution-runtime](../rules/execution-runtime.md)). A Swap
rebuilds every input latch from the new document, so a downstream consumer's held-input latch resets
to its default. An operator that publishes a **held output on change** (emit-on-change) against a baseline it keeps in its box would then see
no change and stay silent, stranding that consumer on the default — a Swap would silently retranspose
a `harmony`-driven voice. Such an operator clears its publish baseline here (`self.last = None`) so the
first post-swap block re-asserts the current value; `harmony.rs` is the exemplar. RT-safe: it runs in
the render-callback transplant loop, so it must not allocate. Signal outputs (refreshed every block)
and event outputs (append-only) need nothing.

- **`descriptor()`** — see below. The single source of an operator's ports and metadata.
- **`process(io)`** — the only realtime path. **Allocation-free.** Read inputs, write outputs
  through the `Io` view, by the contract's typed handles — the handle's form decides
  the read/write shape, and a wrong-form access does not compile. The per-form read/write
  shapes — the buffer-presence invariant and `io.varying`, held reads defaulted by the handle,
  an enum read as the real vocab type, `Harmony`'s default, `EventStream<Note>`/`Stamped<Note>`
  — live once, in the guide's type-system table
  ([authoring.md#type-system](authoring.md#type-system)); read them there. What the table
  doesn't carry, because only an operator author sees it:
  - **Writer semantics differ by form.** A held Value/`Harmony` write (`io.write(OUT) ->
    MsgWriter`, `.set(frame, v)`) is **deduped + last-write-wins** per frame — an unchanged
    value emits nothing. An Event write (`io.write(OUT) -> EventWriter`, `.emit(frame,
    payload)`) is **append-only** — no dedup, no LWW: a chord's tones at one frame all
    survive. A Signal write (`io.write(OUT) -> &mut [f32]`) fills the buffer in place.
  - **No `enum_index`/`from_index` on the hot path** — an enum handle reads the real vocab
    type directly (`io.read(IN_WAVEFORM) == Waveform::Saw`), constant for the (sub)block.
  - **Internal wires are addressless** — routed by connection, not name. Exemplars:
    `sequencer.rs` / `snap.rs` for `Note` events; the Voicer and `snap.rs` read `Harmony`,
    `harmony.rs` emits it.
- **`spawn()`** — usually `Box::new(Self::new())`. Resets per-Voice state only. A resource-bearing
  operator carries its binding (the `Arc<ResourceStore>` + resolved handle) forward while resetting
  playback state, so every Voice shares the decoded data — see `sample.rs`.
- **`bind_resources(store, refs)`** — the two-phase-init hook for operators depending on
  **external decoded data**. The
  loader resolves+decodes the document's `resources` table into a shared `ResourceStore` and calls
  this hook on each node declaring a resource slot. Default no-op. `sample.rs` is the template.

State that must persist across blocks lives on the struct (e.g. an oscillator's phase). Hold an
accumulating phase in `f64` so it doesn't drift over a long session (see `lfo.rs`).

<a id="descriptor-macro"></a>
## The Descriptor (`crates/reuben-core/src/descriptor.rs`)

An operator's self-description, separate from `process` — the seat of "good button",
serialization, connection checking, and AI grounding
([agent-mcp](../rules/agent-mcp.md)).

You declare it **once**, in an `operator_contract!` call
([composition-operators](../rules/composition-operators.md)). The macro plants, at module scope,
the **typed `IN_`/`OUT_` handles** (`In<form>`/`Out<form>` consts whose type is the port's form and
whose value carries the declared default), the `C_*` constant ordinals, **and** an
inherent `fn contract() -> Descriptor` from the same tokens, so handles and descriptor can't drift. An `enum` port **references a shared *vocab* enum** by
name (`enum(FilterMode)`) — it generates no per-op type; the descriptor is single-sourced off the
vocab type's `FilterMode::enum_meta(name)`. The trait's `descriptor()` delegates:

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
    fn descriptor() -> Descriptor { Self::contract() }   // one-liner delegate
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
- **`name: note`** / **`name: harmony`** / **`name: pitch`** — `Note` (Event) / `Harmony` (Value) /
  `Pitch` (Value) ports (`Port::note` / `Port::harmony` / `Port::pitch`). `pitch` is a held `Pitch`
  leaf — the wire-internal output an `unpack_<type>` op emits, consumed by `pitch2freq`.
- **`name: arg`** — a **type-agnostic pass-through** (issue #141): carries *any* `Arg` as a raw
  Event stream (`Port::arg`), read via its `In<Raw>` handle (`io.read(IN)` yields undecoded
  `&Arg` payloads) and re-emitted through the sink's local `Out<Raw>` tap handle
  (`io.write(OUT_TAP)` in `osc_out.rs`).
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
  (the Voicer's `voices`); the loader
  routes them to the patch's `config` block. Constants keep bare `usize` `C_*` ordinals — they are
  never read in `process`, so they get no handle.

Other notes:

- An operator with no explicit `type_name:` takes the snake_case of its struct name; pass
  `type_name: "sample"` when they diverge (e.g. `SamplePlayer`).
- **Ports are referenced by name** in the JSON format, not by index — names are the stable
  contract the rig builder wires against. The macro computes the ordinals.
- **No exceptions:** every operator declares its contract through the macro and delegates via
  `Self::contract()` (`output`, the last hand-written descriptor, folded in so its
  ports get typed handles too). One operator per module (the old `math.rs` was split
  one-file-per-op).
- **Pointwise number ops use a higher-level macro.** `add`, `mul`, `power`, `map` are each a single
  `crate::number_operator_contract!(..)` call over one scalar fn, which generates **both carriers**
  (`*F32Value` + `*F32Signal`) — their contracts, a `ValueOp`/`SignalOp` impl naming the contract's
  handle consts, registration, and a defaults-are-data test. Each carrier's name is a `pub type`
  alias for the matching **shell** ([`operator::shell`](../../crates/reuben-core/src/operator/shell.rs)),
  which owns `process`; the macro does not emit one. The criterion:
  an op is macro-eligible iff it is **stateless pointwise** (output sample = fn of this sample's
  inputs only) **and** every operand is a number or held enum mode. `differentiate`/`integrate` are
  **stateful** (they carry state across blocks), so they stay hand-written `operator_contract!` ops
  and are **signal-only** (a value form would shatter their continuous one-sample-`dt` stream).
- **Product vocab types get their `unpack` from a census macro.** `crate::unpack_op!(Note { pitch:
  pitch, velocity: f32 })` in `operators/unpack.rs` generates an `unpack_<type>` op that reads the
  whole value as a `Note` event on `in` and emits each field as a held `Value` — the Event→Value
  latch expressed as a patchable node.
  Like `number_operator_contract!` it reuses the shared contract internals, so the op is
  indistinguishable in shape from a hand-written one and self-registers via `inventory`. The census
  is one greppable file, but the macro's input event form is currently fixed to `note` (`Note` is the
  only event-carried product vocab type today), so unpacking a *different* product type is a census
  line **plus** teaching the macro that type's event input — not a one-line edit alone.
- **Polyphony** is not a per-operator concern: there is no Lane fan-out. The **Voicer** is
  a single-Voice operator that hosts N voice sub-patches — a voice is a standalone Instrument
  (instrument-resource, declared `resources: { voice }`) with an `interface { inputs, outputs }`
  boundary (`freq`/`gate` in, `audio`/`active` out). The loader builds the patch `voices` times and
  `bind_voices` them; the Voicer instantiates each into its own sub-`Plan` at `on_instantiate`, drives
  per-voice `freq`/`gate`, and sums their audio. See `voicer.rs` and `instruments/voices/*.json`.

### Enum over the wire: symbol primary, index fallback

The wire contract lives in the guide
([authoring.md#addressing](authoring.md#addressing)): an enum input is addressed by its
variant-name **symbol** (`/filt/mode "Hp"`, `"mode": "Hp"`), a bare in-range integer index is
accepted as a fallback, and an unknown symbol or out-of-range index is an **error** — it never
silently snaps to the default. On the code side, resolution lives in one place —
`EnumMeta::resolve` — single-sourced with the shared *vocab* enum type's
`VARIANTS`/`from_symbol` (both generated by `#[derive(ArgValue)]`), so an operator author
never hand-writes symbol/index handling.

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
   submission `Registry::builtin()` gathers,
   so there is **no central list to edit**. (`grep -rn register_operator! operators/` is the census.)
4. **Test** in the operator module, test-first, with
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
seam for the "agents author new Operators in Rust" goal ([agent-mcp](../rules/agent-mcp.md)).

## Invariants as they bind an operator author

The engine-wide invariants — determinism, RT-safe Render, the OSC-only core, the
single-writer boundary — live in the guide's
[Invariants you must not break](authoring.md#invariants-you-must-not-break). Two of them land
directly on the code you write here:

- **Determinism** binds `process` like everything else — check the guide's bullet before
  reaching for a clock or a random source.
- <a id="rt-safe-render"></a>**RT-safe Render** — the invariant's normative statement and its
  enforcing test live in the guide's bullet; `process` sits squarely inside it. How it lands
  on an operator author: all scratch is preallocated and reused (including the materialize
  buffers for `Value → Signal` edges and the Voicer's per-voice sub-`Plan` arenas); routed
  events are zero-copy.
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
    a committed benchmark ([web-product-process](../rules/web-product-process.md)) proving it.
