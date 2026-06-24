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

## Two things flow on edges ([ADR-0001](../adr/0001-unified-block-graph-execution.md))

- **Signal** — a continuous audio-rate float buffer, one block per Channel. CV and audio
  are the same type; there is no separate control-rate signal. (`signal.rs`)
- **Message** — a discrete, OSC-shaped payload: address path + typed args + sample-accurate
  timetag. Notes, chords, triggers, gestures, param values, all external I/O. An internal
  Message and an external OSC packet are the same shape. (`message.rs`)
- **Context** — a latched tonal-context struct (key/scale/chord) that rides the Message wire
  as a struct-valued read service: a `context` Operator publishes it, followers read "the
  current value" via `io.context` ([ADR-0015](../adr/0015-latched-context-read.md)). Not a
  third edge type — a third read accessor over the one Message wire. (`context.rs`)

## The Operator contract (`crates/reuben-core/src/operator.rs`)

Operators are authored **single-Lane** ([ADR-0010](../adr/0010-single-lane-operators.md)):
you write one mono, single-Voice stream a (sub)block at a time, and the engine fans it out
across Lanes (Voice × Channel) with per-Lane state. The trait is three methods:

```rust
pub trait Operator: Send {
    /// Static self-description (ports + param metadata). Drives serialization,
    /// connection checking, good-button controls, and AI grounding.
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

- **`descriptor()`** — see below. The single source of an operator's ports and params.
- **`process(io)`** — the only realtime path. **Allocation-free.** Read inputs/params,
  write outputs through the `Io` view (`crates/reuben-core/src/operator.rs`):
  - `io.input(port) -> Option<&[f32]>`, `io.output(port) -> &mut [f32]` — Signal ports,
    each exactly `io.frames()` long.
  - `io.param(slot) -> f32` — constant for the whole call (the engine block-slices at
    Message boundaries, [ADR-0011](../adr/0011-message-delivery-and-timing.md), so you
    just read "my current value").
  - `io.events() -> &[Event]` — for event operators (Voicer, Clock): zero-copy views of
    routed Messages, address local to the node, segment-relative `frame`.
  - `io.emit(port, addr, args, frame)` — emit a Message onto a **Message output port**
    ([ADR-0014](../adr/0014-internal-message-graph.md)), e.g. a sequencer emitting `degree`
    into a Voicer. `addr` is a `&'static str` and the wired edge does the routing, so a
    note emit allocates nothing; `frame` is segment-relative. Delivered as an `Event` to
    nodes downstream this block. Emission is single-Lane (Lane 0 only) — pre-fan-out. See
    `sequencer.rs` as the template.
  - `io.context(port) -> Context` — read the latched tonal **Context** on a Context input
    port ([ADR-0015](../adr/0015-latched-context-read.md)): the current key/scale/chord,
    constant for the (sub)block (the engine slices at context changes), carrying the
    resolver (`hz`/`snap`/`chord_tone`). Unconnected → the C-major/12-TET default. The
    Voicer and `snap.rs` are the templates. A `context` Operator writes the other side with
    `io.publish_context(port, frame, ctx)` (single-Lane, like `emit`) — see `context.rs`.
  - `io.lane()` / `io.lanes()` — most operators ignore these; an *expander* like the
    Voicer uses them to emit one Voice's output per call.
- **`spawn()`** — usually `Box::new(Self::new())`. Resets per-Lane state only; the engine
  applies params separately. A resource-bearing operator instead carries its binding (the
  `Arc<ResourceStore>` + resolved handle) forward through `..Self::default()` while resetting
  playback state, so every Voice shares the decoded data — see `sample.rs`.
- **`bind_resources(store, refs)`** — the two-phase-init hook for operators that depend on
  **external decoded data** ([ADR-0016](../adr/0016-sample-player-and-resource-store.md)).
  Construction is zero-arg and type-erased, so a sample player can't take its audio as a
  constructor arg; instead the loader resolves+decodes the document's `resources` table into
  a shared `ResourceStore` and calls this hook on each node that declares a resource slot.
  Default no-op. The descriptor declares the slot; the RT read goes through the store's pure
  `(id, channel, frame)` accessor (bank-streaming-safe). `sample.rs` is the template.

State that must persist across blocks lives on the struct (e.g. an oscillator's phase).
Hold accumulating phase in `f64` so it doesn't drift over a long session (see `lfo.rs`).

## The Descriptor (`crates/reuben-core/src/descriptor.rs`)

An operator's self-description, separate from `process` — the seat of "good button",
serialization, connection type-checking, and AI grounding
([ADR-0004](../adr/0004-ai-authorability-first-class.md)).

You declare it **once**, in an `operator_contract!` call
([ADR-0025](../adr/0025-single-source-operator-contract.md)). The macro plants, at module scope,
the `IN_/OUT_/P_` index consts **and** an inherent `fn contract() -> Descriptor` from the same
tokens — so the consts and the descriptor can't drift. The trait's `descriptor()` delegates to it:

```rust
crate::operator_contract!(Lfo {
    outputs: { out: signal },                 // name: signal | message | context, per kind
    params:  { rate:   { 0.01..=20.0, default 5.0, "Hz", exp },   // min..=max, default, unit, lin|exp
               depth:  { 0.0..=1000.0, default 10.0, "", lin } },
    // resources: { sample },                 // optional — see ADR-0016
    lanes: inherit,                           // or from_param(<param>) for an expander
});

impl Operator for Lfo {
    fn descriptor() -> Descriptor { Self::contract() }   // one-liner delegate (ADR-0025)
    // process / spawn ...
}
```

An operator with no explicit `type_name:` takes the snake_case of its struct name; pass
`type_name: "sample"` when they diverge (e.g. `SamplePlayer`). The macro **does not** own the whole
operator — `process`/`spawn` stay hand-written and read `io.param(P_RATE)` against the planted const.

- **Ports** are referenced by **name** in the JSON format, not by index — names are the
  stable contract the rig builder wires against. Per-kind ordinals (signal/message/context are
  separate index spaces, [ADR-0010](../adr/0010-single-lane-operators.md)) are computed by the
  macro.
- **Params** carry range, default, unit, and response curve (`lin`/`exp`) — enough to render a
  control that can't sound bad and to ground an agent. The index consts (e.g. `P_RATE`) the macro
  emits are what `process` reads against.
- **Exceptions:** `math.rs` (five operators in one module) and `context.rs` / `sequencer.rs`
  (param banks built by a loop) keep a hand-written `descriptor()` — the macro is for the
  static-contract, one-operator-per-module common case.
- **`LaneRule`** — `Inherit` (Lane count = max of input Lane counts; the default) or
  `FromParam(slot)` (this operator *expands*, producing that many Lanes; the Voicer is the
  canonical expander). Read once at Instantiate — it's structural.

### The one-port-one-type rule ([ADR-0017](../adr/0017-playable-surface-and-control-domain.md))

A functional input is **exactly one port of one type** — never a param *and* a CV (Signal)
port for the same quantity.

- **Favor a Signal input** where audio-rate modulation is musical (freq, cutoff, amp, pan);
  use a **Message param** for discrete/structural controls (waveform, mode, voice count, room
  size). In doubt, favor the higher-resolution (Signal) input.
- A **Signal input port carries an unwired default scalar** — static use ("cutoff sits at
  2000") needs no upstream node; the default is the one scalar that survives from the old
  "param." Read inputs as `io.input(port) -> Option<&[f32]>`, falling back to the default when
  unwired (the oscillator's `freq` is the template).
- To drive a Signal input from **Messages**, the author inserts the explicit **Message→Signal
  converter** (its `mode` param picks snap/slew/smooth/glide). Interpolation/smoothing logic
  lives *once* in that converter — never re-implement it per operator.
- Cross-domain wiring (Message port → Signal port, or vice-versa) is a **type error**
  (`PortKindMismatch`); resolve it with an explicit converter, never an implicit coercion.

There is therefore no "combine a param and a CV value at one port" question — base-plus-
modulation is built explicitly with an `add` operator in the relevant domain.

## Adding an Operator

1. **Create** `crates/reuben-core/src/operators/<name>.rs` — a struct + `impl Operator`.
   Declare the contract once with `crate::operator_contract!(..)` (it plants the `IN_/OUT_/P_`
   index consts + the `Descriptor`, [ADR-0025](../adr/0025-single-source-operator-contract.md)) and
   delegate `fn descriptor() -> Descriptor { Self::contract() }`. Follow `lfo.rs` (simplest source
   op) or `delay.rs` (input + state) as a template. (`reuben scaffold-operator` writes this shape
   for you.)
2. **Wire the module** in `crates/reuben-core/src/operators/mod.rs`: `pub mod <name>;`
   and `pub use <name>::<Type>;`.
3. **Self-register** by adding one line at the operator's module top level, after its
   `impl Operator` block: `crate::register_operator!(<Type>);`. This submits the type to a
   compile-time `inventory` slice that `Registry::builtin()` gathers ([ADR-0024](../adr/0024-compile-time-operator-registration.md)),
   so there is **no central list to edit** — operators self-register where they're defined, and
   parallel branches no longer collide in `registry.rs`. (`grep -rn register_operator! operators/`
   is the census of built-ins.)
4. **Regenerate the schema** so JSON validation knows the new type/params:
   ```sh
   cargo run -p reuben-core --example gen_schema
   ```
   Commit the updated `crates/reuben-core/schema/instrument.schema.json`. The
   `schema_is_in_sync` test fails if it's stale.
5. **Test** in the operator module, test-first. At minimum cover: output correctness,
   phase/state continuity across back-to-back blocks (one whole block == two half-blocks
   sharing the instance), and that a `spawn()`ed copy starts fresh. See `lfo.rs` tests.

Embedders can add their own types without touching the core via `Registry::register` — the
seam for the "agents author new Operators in Rust" goal ([ADR-0004](../adr/0004-ai-authorability-first-class.md)).

## The Instrument format (`crates/reuben-core/src/format.rs`)

An Instrument is plain JSON data: `nodes` (operator `type` + `address` + optional `params`
overrides + optional `doc`), `connections` between named ports, and master `outputs`.
Ports are referenced by name; addresses are OSC paths, unique within the instrument and the
routing prefix for that node's params (so `/delay/time` sets the `time` param of the node at
`/delay`). `format::load` resolves types via a `Registry` and returns a `Graph`. Loading is
an authoring step — it lives in the portable core but never runs on the audio thread. See
`instruments/*.json` for worked examples.

A document may also carry a top-level `resources` table (logical id → source path) that
resource-bearing nodes reference by a `sample` field
([ADR-0016](../adr/0016-sample-player-and-resource-store.md)). Resolving + decoding those
needs a `ResourceResolver`, so use `format::load_instrument(json, registry, resolver)` — it
returns the `Graph` plus any non-fatal `LoadWarning`s (a missing/undecodable sample degrades
to silence). `instruments/sampler.json` is the worked example; `reuben-native` supplies a
filesystem WAV resolver.

A node may also carry an optional **`control`** block
([ADR-0018](../adr/0018-control-surface-generation.md)) — surface metadata marking it
player-facing: a `label` (required) plus optional `unit`/`widget`/range, a `param` (to bind a
specific param instead of the node address), or `widget: "note-toggle"` with a `note`/`port`
(a play toggle). It is **opaque to the engine** — an untyped passthrough on `NodeDoc` that
round-trips through load/save but is never read at runtime; the [`control-surface`
skill](../../.claude/skills/control-surface/SKILL.md) reads it to generate a TouchOSC surface.
A `control` value is a single spec or an array (a multi-param node like a sequencer's steps).
`instruments/good-button.json` is the worked example.

## Addressing

Every node has an OSC **address**, derived from graph structure by default. A Message
targets a node by address prefix; the local remainder becomes the `Event` address (e.g.
`/voicer/note` under node `/voicer` arrives as event `note`). Full wildcard dispatch
(`/drums/*/decay` hitting many nodes at once) is designed but not built yet — today a
Message targets at most one node ([ADR-0005](../adr/0005-osc-namespace-and-wildcards.md)).

## Invariants you must not break

- **Determinism** — output is bit-identical regardless of executor or thread interleaving
  ([ADR-0001](../adr/0001-unified-block-graph-execution.md)). No wall-clock, no RNG without
  a seeded, plan-owned source.
- <a id="rt-safe-render"></a>**RT-safe Render** — `render_block` is allocation-free after
  warmup, asserted by `crates/reuben-core/tests/rt_safe.rs`. Code that runs on the audio
  render thread(s) — the **hot** path — must not allocate, lock, or block, and must not
  panic. All scratch is preallocated and reused; routed events are zero-copy.
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
