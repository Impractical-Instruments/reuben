# reuben — Architecture

The design end to end. This narrative ties together the glossary ([CONTEXT.md](../CONTEXT.md)) and the decisions ([docs/adr/](adr/)); open work and the design backlog live in the GitHub issue tracker. Capitalized terms (Operator, Plan, Voice…) are defined in the glossary.

> **This describes the target design, not the current build state.** For what actually exists today, see the [README](../README.md) status line. Where a described mechanism isn't built yet, it's flagged inline.

## What reuben is

A configurable musical instrument. You build behavior by patching together **Operators** — small units that each do one simple thing — into **Instruments**, and Instruments into a **Rig** (a full playable system). Beginners start with **Toys**: ready-made Instruments/Rigs that play instantly. The same engine that makes music can drive lights, video, or a game engine, because the data flowing through it is general.

## Design pillars

- **Good button.** Every control is hard to make sound bad; energy in produces juicy musical feedback out; easy defaults always exist. This is enforced by mechanism (rich param metadata, snap-to-scale, groove), not hope.
- **Easy to learn, deep to master.** Toys and defaults on the surface; recursive composition and full control underneath. The same gradient appears everywhere (a global Clock you can override with Clock Operators; a default Tuning you can replace; a curated control surface over structural addresses).
- **AI-authorable, first-class.** Agents (for developers, patchers, and end users) can read the system and author Operators, Instruments, and Rigs. One recursive model, self-describing Operators, a JSON format with a generated schema. See [ADR-0004](adr/0004-ai-authorability-first-class.md).
- **OSC is the lingua franca.** Internal Messages and external OSC are the same idea (reconciled by explicit boundary conversion). Other protocols convert at the boundary. See [ADR-0007](adr/0007-osc-only-core.md).
- **Portable core, removable native layer.** The realtime core is OS-free Rust; audio I/O, threads, and protocol adapters live in a thin native layer that can be swapped for a game engine or DAW host. See [ADR-0012](adr/0012-boundary-and-threading.md).

## The model: Operators → Instruments → Rigs

Composition is **recursive** ([ADR-0003](adr/0003-recursive-composition.md)): there is one concept — a graph of nodes with typed ports — at every scale. An Instrument is a named subgraph that exposes boundary ports, so it can be reused inside another Instrument or Rig *as if it were an Operator*. Nesting is an authoring concept only; at runtime everything is inlined into one flat graph.

Two things flow on the edges ([ADR-0001](adr/0001-unified-block-graph-execution.md)):

- **Signal** — a continuous audio-rate float buffer (one block per Channel). CV and audio are the same thing. There is no separate control-rate signal.
- **Message** — a discrete, OSC-shaped payload (address + exactly one `Arg` + a sample-`frame` timestamp). Notes, chords, triggers, gestures, param values, dense audio (as an `f32_buffer` Arg), and all external I/O. Sub-audio-rate control travels as Messages (the Max/PD model).

Everything is **addressable** by an OSC path derived from graph structure, plus a curated public control surface an Instrument may expose. Wildcards (`/drums/*/decay`) are designed to dispatch internally as well as externally — which makes Good Buttons and effect racks fall out for free. See [ADR-0005](adr/0005-osc-namespace-and-wildcards.md). *(Wildcards not built yet: today a Message targets at most one node, matched by address prefix. The generated surface exists — per-node `control` metadata → a TouchOSC layout via the `control-surface` skill, [ADR-0018](adr/0018-control-surface-generation.md) — and the first-class boundary declaration landed with nesting: an Instrument's `interface` block names its public ports, with presentational metadata inherited from the inner ports and per-field overridable, [ADR-0034](adr/0034-instrument-nesting.md) §4.)*

## Execution and runtime

**One unified graph, processed in blocks.** Each block, Messages and Signals are computed in a single dependency-ordered pass — a **single static topological schedule** (not separate control/audio phases). Threads are not owned by the core: it dispatches the schedule through a pluggable executor. The MVP ships a **serial** executor; a parallel executor — independent branches run concurrently, coalesced into cost-weighted clusters, recomputed only when the graph changes — is designed to slot in behind the same trait. Output is **bit-deterministic** regardless of executor or thread interleaving. See [ADR-0001](adr/0001-unified-block-graph-execution.md).

**Lifecycle — Build → Swap ⇄ Render, over a Plan** ([ADR-0009](adr/0009-graph-lifecycle.md)):

- **Build** — compile the engine binary. Operator *types* exist; nothing user-specific.
- **Swap** — the one runtime transition that changes the graph: **Instantiate** a new **Plan** off the audio thread (allocate the delta, build the parallel schedule), atomically install it at a block boundary, migrate surviving Operators' state, reclaim the old Plan. The first build is just a Swap from the empty Plan — no special cold-start path.
- **Render** — execute the current Plan per block on the audio thread. Hard realtime, allocation-free: the [`Renderer`](../crates/reuben-core/src/render.rs) preallocates its edge-buffer arena and all per-block scratch at construction and reuses them; routed events are zero-copy views onto the caller's Messages, so a warmed-up `render_block` performs no heap allocation even while delivering notes (asserted by `crates/reuben-core/tests/rt_safe.rs`). Playing notes and turning knobs happen here.

**Boundary and threading** ([ADR-0012](adr/0012-boundary-and-threading.md)): one writer of structure (the **Coordinator**), one reader at Render (an immutable Plan), everything else lock-free message passing.

```
            ┌──────────────────────────── reuben ────────────────────────────┐
 TouchOSC   │                                                                 │
 Max / Pd ──┼─OSC─▶┌───────────────┐  commands   ┌────────────────┐  (non-RT) │
 MIDI/Link  │      │ I/O & control  │────────────▶│  Coordinator   │           │
 (adapters) │      │   adapters     │  params     │  owns graph;   │           │
            │      └───────┬────────┘ (lock-free  │  Instantiate,  │           │
            │              │           Message Q)  │  Swap, reclaim │           │
            │              ▼                       └───────┬────────┘           │
            │      ┌────────────────────────────┐ new Plan │ (atomic Swap)     │
 speakers ◀─┼audio─│         RENDER (RT)         │◀─────────┘                   │
            │      │ reads immutable Plan,       │                             │
            │      │ drains queues, dispatches   │──▶ executor pool            │
            │      │ clusters; allocation-free   │    (serial → parallel)      │
            │      └────────────────────────────┘                             │
            │   metering / introspection ▲ (lock-free Render→outside queue)    │
            └─────────────────────────────────────────────────────────────────┘
   portable: Render + Coordinator + Plan      │      native/removable: I/O adapters + executor
```

## Operators

An Operator carries a self-describing **descriptor** (ports + rich metadata: range, default, units, response curve) separate from its process function. Each functional input is one **`Input`** declared by its **`Arg` type** — an OSC primitive (`F32`/`I32`/`Str`), a shared `vocab` concrete type (`Note`, `Harmony`, `FilterMode`, …), or an `f32_buffer` (a dense per-sample `Signal`). A port's **form** ([ADR-0031](adr/0031-float-resolves-to-value-or-signal-by-wiring.md)) follows from how it is declared: a bare scalar (`f32`) is a held **Value** (a latched single value), an `f32_buffer` is a **Signal** (a per-sample buffer, optionally seeded by a `meta` default), and a `vocab` struct like `Note` is an **Event** (a sparse frame-stamped stream). The planner runs a **local per-wire check**: like→like connects directly, `Value→Signal` materializes (sample-and-hold into a buffer), and `Signal→Value` is a hard error (it names the missing converter). Instantiate-time configuration (e.g. voice count) is a separate `Constant`, declared by the contract's `constant:` keyword ([ADR-0032](adr/0032-voicer-hosts-voice-subpatches.md)). The descriptor is the seat of "good button" (auto-generated controls that can't sound bad), of serialization, of type-checking connections, and of AI grounding.

Operators are **single-Voice** by default ([ADR-0010](adr/0010-single-lane-operators.md)): the author writes one mono, single-Voice stream (a block at a time). Polyphony comes from *above* the operator layer — the **Voicer** ([ADR-0032](adr/0032-voicer-hosts-voice-subpatches.md)) hosts N **voice sub-patches** (a voice is a standalone Instrument referenced by a `voice` resource, declaring an `interface { inputs, outputs }` boundary), drives each voice's `freq`/`gate`, allocates notes across the pool, and sums their audio. Per-Voice state lives inside each sub-patch's own sub-Plan, rendered re-entrantly with its own arena — so an operator never carries per-Voice copies. *(This replaces the original per-**Lane** Voice×Channel fan-out, now removed.)* Other cross-cutting work (mixing, panning) likewise lives above the operator layer via deterministic fan-in connections and Channel-aware boundary constructs.

**Voices** (independent sounding instances, from a pre-allocated pool bounded at Instantiate) are distinct from **Channels** (n-channel master signal paths, [ADR-0026](adr/0026-v1-finish-line-osc-out-and-stereo.md)); a stereo Voice spans two Channels.

**Message delivery** ([ADR-0011](adr/0011-message-delivery-and-timing.md)) is sample-accurate but author-transparent: the engine **block-slices** at Message boundaries so a single-Voice author just reads "my current value" while a knob change lands at the exact sample. The read/write surface is two return-type-dispatched verbs ([ADR-0031](adr/0031-float-resolves-to-value-or-signal-by-wiring.md)): **`io.input::<T>(port)`** (a `&[f32]` reads a Signal buffer, a scalar/enum/`Harmony` reads the held Value, a `Note` iterates the sparse Event stream — each item a typed payload + segment-relative frame) and **`io.output::<T>(port)`** (an `f32` returns a `MsgWriter` for held/sparse writes, a `&mut [f32]` a Signal buffer, a `Note` an append-only `EventWriter`). Event-oriented Operators (the Clock, the Voicer, the sequencer) reason in events and read `io.input::<Note>`. Operators **emit** Messages onto wired Message edges ([ADR-0014](adr/0014-internal-message-graph.md)) — a sequencer feeding a Voicer — via these output writers (internal wires are **addressless**, routed by connection), and a `harmony` Operator emits a latched tonal-context struct read back via `io.input::<Harmony>` ([ADR-0015](adr/0015-latched-context-read.md), the `Harmony` `Arg`), the struct-valued sibling of a knob.

## Musical layer

- **Clock** ([ADR-0006](adr/0006-clock-and-musical-time.md)): a global default Clock so everything grooves together out of the box; Clocks are also Operators for polytempo/generative timing. The Clock provides base timing only — groove, swing, and feel are separate Operators. Timetags default to musical time, resolved against the Clock at dispatch.
- **Pitch and tuning** ([ADR-0008](adr/0008-pitch-and-tuning.md)): symbolic Pitch (scale-degree primary, float-MIDI-note available) resolved to frequency by a **Tuning**. Tunings import from Scala `.scl`/`.kbm`, supporting any non-Western or user-defined system; 12-TET is just the default. The active Tuning rides the **tonal-context** bus alongside key/scale/chord, queried continuously, so retuning while notes sound works.
- **Tonal-context bus** ([ADR-0013](adr/0013-tonal-context-bus-mechanics.md), [ADR-0015](adr/0015-latched-context-read.md)): the key/scale/chord is a latched `Copy` struct a `harmony` Operator emits and followers read via `io.input::<Harmony>` (the `Harmony` `Arg`) — the resolver (`hz`/`snap`/`chord_tone`) lives in that one struct, so a follower stays dumb. Degree notes resolve to Hz through it (a held line **re-spells live** on a key/scale change), a `snap` Operator quantizes arbitrary pitch to the nearest in-scale degree, and changes are sample-accurate on the same timeline as notes. *(Built for 12-TET; the Scala-tuning swap rides the same step-space seam and is deferred.)*

## Resources (sample data)

Most Operators are pure functions of params + edges; the **sample player** is the first to
depend on **external decoded audio** ([ADR-0016](adr/0016-sample-player-and-resource-store.md)).
An instrument document carries a top-level `resources` table (logical id → source); decoded
audio lives in a central `ResourceStore` the Coordinator builds at load and Render reads
immutable. A type-erased Operator receives it through a two-phase `bind_resources` hook
(driven by a descriptor resource slot), and the read path is a **pure `(id, channel, frame)`
accessor** — resident in v1.1, the same signature the future streaming "audio bank" reuses,
so determinism holds (a bank that falls behind underruns, never substitutes silence). Codecs
stay out of the portable core: a `ResourceResolver` seam in the native layer owns filesystem
IO and WAV decode. A missing or undecodable sample **degrades to silence with a load
warning** rather than crashing a live rig; structural/wiring errors stay fatal.

## Boundary protocols

The core speaks only OSC-shaped Messages. MIDI, Ableton Link, OSC tempo sync, and future protocols are isolated, removable adapters that convert to/from OSC at the I/O boundary ([ADR-0007](adr/0007-osc-only-core.md)). Each adapter is part of the removable native layer.

## MVP and beyond

The MVP is a headless "it makes a sound" spine: the portable-core / native-crate split, Signal + Message, the Plan + Instantiate→Render loop, single-Voice operators, determinism, a serial executor behind the real interface, the core Operators (oscillator, envelope, filter, Voicer, output, Clock), OSC-in from TouchOSC/Max, default 12-TET. V1.1 added music Operators (delay, reverb, LFO, sequencer, sample player), the internal message graph (operators emit Messages), the tonal-context bus (a `harmony` Operator + degree resolution + a `snap` Operator), and the resource store (decoded sample data as a shared, bank-ready read service). V1.2–V1.5 built out the playable surface: the math/curve operator family and Good Buttons from composition ([ADR-0017](adr/0017-playable-surface-and-control-domain.md), [ADR-0029](adr/0029-math-family-dense-float-one-file-per-op.md)), generated TouchOSC control surfaces ([ADR-0018](adr/0018-control-surface-generation.md)), the V1.3 launch Toys (chord-player, groovebox, strum-harp; [ADR-0022](adr/0022-the-toys.md)), the envelope as a linear-CV source shaped by curve ops ([ADR-0027](adr/0027-envelope-emits-cv-and-curve-ops.md)), and the v1 finish line — an `osc_out` node and stereo via a `pan` op ([ADR-0026](adr/0026-v1-finish-line-osc-out-and-stereo.md)). Under all of it, the data model has collapsed to a single `Message`/`Arg` type ([ADR-0030](adr/0030-osc-as-all-data-one-message-type.md)), a numeric port now resolves to a held **Value** or a **Signal** buffer by how it's declared and wired ([ADR-0031](adr/0031-float-resolves-to-value-or-signal-by-wiring.md)), and the **Voicer** hosts standalone **voice sub-patches** as its pool — retiring the per-Lane fan-out model ([ADR-0032](adr/0032-voicer-hosts-voice-subpatches.md)). General instrument-in-instrument nesting has landed ([ADR-0034](adr/0034-instrument-nesting.md)): a built-in **`subpatch`** node references another instrument through a `patch` resource slot; at build the child resolves recursively (cycle-guarded), its `interface` synthesizes into the node's **boundary face** — types inherited from the inner ports and covered by the ordinary wire check, presentational metadata (label/unit/widget/range) inherited and per-field overridable — and the child inlines into the flat parent graph under the node's address prefix, the node dissolving to zero runtime cost. `reuben describe <patch.json>` introspects the boundary a host wires against; the instrument-library story (resolution/naming/versioning) is the trailing pass. For the code-level operator contract and how to add one, see [docs/agents/authoring.md](agents/authoring.md).

## Decision index (ADRs)

- [0001 — Unified block graph with a static parallel schedule](adr/0001-unified-block-graph-execution.md)
- [0002 — Rust for the core and native layer](adr/0002-rust-core.md)
- [0003 — Recursive (fractal) composition](adr/0003-recursive-composition.md)
- [0004 — AI-agent authorability as a first-class constraint](adr/0004-ai-authorability-first-class.md)
- [0005 — OSC namespace: hybrid addressing with wildcard dispatch](adr/0005-osc-namespace-and-wildcards.md)
- [0006 — Clocking and musical time](adr/0006-clock-and-musical-time.md)
- [0007 — OSC-only core; protocol adapters at the boundary](adr/0007-osc-only-core.md)
- [0008 — Pitch and tuning: symbolic pitch + Scala-based Tuning](adr/0008-pitch-and-tuning.md)
- [0009 — Graph lifecycle: Build → Swap ⇄ Render over a Plan](adr/0009-graph-lifecycle.md)
- [0010 — Single-Lane Operators; cross-cutting work above the operator layer](adr/0010-single-lane-operators.md)
- [0011 — Message delivery and sample-accurate timing](adr/0011-message-delivery-and-timing.md)
- [0012 — Boundary and threading: single-writer Coordinator, read-only Render](adr/0012-boundary-and-threading.md)
- [0013 — Tonal-context bus: mechanics (latched context Operator, snap, sample-accurate timing)](adr/0013-tonal-context-bus-mechanics.md)
- [0014 — Internal message graph: Operators emit Messages on wired edges](adr/0014-internal-message-graph.md)
- [0015 — Latched context read: a struct-valued read service over the Message wire](adr/0015-latched-context-read.md)
- [0016 — Sample player and the resource store: decoded audio as a shared, bank-ready read service](adr/0016-sample-player-and-resource-store.md)
- [0017 — The playable surface: Message-first control, one-port-one-type, Good Buttons from composition](adr/0017-playable-surface-and-control-domain.md)
- [0018 — Generated control surfaces: the `control` block, a `map` resting default, and a TouchOSC skill](adr/0018-control-surface-generation.md)
- [0019 — Performance benchmarking: two layers, a deterministic CI gate, compare-against-base](adr/0019-performance-benchmarking.md)
- [0020 — Introspection API and the Patcher skill: `describe` + `validate`, built on the loader](adr/0020-introspection-and-patcher-skill.md)
- [0021 — Scaffolding a new Operator: the `scaffold-operator` subcommand and the `create-operator` skill](adr/0021-scaffold-operator-and-create-operator-skill.md)
- [0022 — The launch Toys (V1.3): three beginner instruments built from Operators](adr/0022-the-toys.md)
- [0023 — Pinned toolchain, lockstep MSRV, and shared git hooks](adr/0023-toolchain-pin-and-git-hooks.md)
- [0024 — Compile-time operator self-registration via `inventory`](adr/0024-compile-time-operator-registration.md)
- [0025 — Single-source the operator port/param contract via `operator_contract!`](adr/0025-single-source-operator-contract.md)
- [0026 — The v1 finish line: OSC-out, stereo, and a release workflow](adr/0026-v1-finish-line-osc-out-and-stereo.md)
- [0027 — Envelope emits linear CV; curve ops shape it; VCA is a `mul`](adr/0027-envelope-emits-cv-and-curve-ops.md)
- [0028 — One `Input`, one axis: `shape` (Float/Enum/Harmony/Note); density and delivery follow from it](adr/0028-one-input-shape.md)
- [0029 — Math is a family of dense `Float` ops, one file per op; the `Number` core is retired](adr/0029-math-family-dense-float-one-file-per-op.md)
- [0030 — OSC-as-all-data: one `Message` type, an `Arg` payload, `Signal` as a Buffer-arg (supersedes 0028)](adr/0030-osc-as-all-data-one-message-type.md)
- [0031 — A numeric port resolves to a held Value or a Signal buffer by declaration + wiring; per-wire form check](adr/0031-float-resolves-to-value-or-signal-by-wiring.md)
- [0032 — The Voicer hosts standalone voice sub-patches as its pool; the Lane fan-out model is retired](adr/0032-voicer-hosts-voice-subpatches.md)
- [0033 — Pointwise number ops are generated from one scalar fn by `number_operator_contract!`](adr/0033-number-operator-contract-macro.md)
- [0034 — General instrument-as-operator nesting: a `subpatch` node inlined at plan-build](adr/0034-instrument-nesting.md)
