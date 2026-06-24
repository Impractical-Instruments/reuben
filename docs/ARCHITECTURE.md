# reuben — Architecture

The design end to end. This narrative ties together the glossary ([CONTEXT.md](../CONTEXT.md)) and the decisions ([docs/adr/](adr/)); the open work and design backlog live in the GitHub issue tracker. When a term is capitalized (Operator, Plan, Lane…) it's defined in the glossary.

> **This describes the target design, not the current build state.** For what actually exists today, see the [README](../README.md) status line. Where a described mechanism isn't built yet, it's flagged inline.

## What reuben is

A configurable musical instrument. You build behavior by patching together **Operators** — small units that each do one simple thing — into **Instruments**, and Instruments into a **Rig** (a full playable system). Beginners start with **Toys**: ready-made Instruments/Rigs that play instantly. The same engine that makes music can drive lights, video, or a game engine, because the data flowing through it is general.

## Design pillars

- **Good button.** Every control is hard to make sound bad; energy in produces juicy musical feedback out; easy defaults always exist. This is enforced by mechanism (rich param metadata, snap-to-scale, groove), not hope.
- **Easy to learn, deep to master.** Toys and defaults on the surface; recursive composition and full control underneath. The same gradient appears everywhere (a global Clock you can override with Clock Operators; a default Tuning you can replace; a curated control surface over structural addresses).
- **AI-authorable, first-class.** Agents (for developers, patchers, and end users) can read the system and author Operators, Instruments, and Rigs. One recursive model, self-describing Operators, a JSON format with a generated schema. See [ADR-0004](adr/0004-ai-authorability-first-class.md).
- **OSC is the lingua franca.** Internal Messages and external OSC are the same shape. Other protocols convert at the boundary. See [ADR-0007](adr/0007-osc-only-core.md).
- **Portable core, removable native layer.** The realtime core is OS-free Rust; audio I/O, threads, and protocol adapters live in a thin native layer that can be swapped for a game engine or DAW host. See [ADR-0012](adr/0012-boundary-and-threading.md).

## The model: Operators → Instruments → Rigs

Composition is **recursive** ([ADR-0003](adr/0003-recursive-composition.md)): there is one concept — a graph of nodes with typed ports — at every scale. An Instrument is a named subgraph that exposes boundary ports, so it can be reused inside another Instrument or Rig *as if it were an Operator*. Nesting is an authoring concept only; at runtime everything is inlined into one flat graph.

Two things flow on the edges ([ADR-0001](adr/0001-unified-block-graph-execution.md)):

- **Signal** — a continuous audio-rate float buffer (one block per Channel). CV and audio are the same thing. There is no separate control-rate signal.
- **Message** — a discrete, OSC-shaped payload (address + typed args + sample-accurate timetag). Notes, chords, triggers, gestures, param values, and all external I/O. Sub-audio-rate control travels as Messages (the Max/PD model).

Everything is **addressable** by an OSC path derived from graph structure, plus a curated public control surface an Instrument may expose. Wildcards (`/drums/*/decay`) are designed to dispatch internally as well as externally — which makes meta-effects and effect racks fall out for free. See [ADR-0005](adr/0005-osc-namespace-and-wildcards.md). *(Not built yet: today a Message targets at most one node, matched by address prefix. An early generated surface exists — per-node `control` metadata → a TouchOSC layout via the `control-surface` skill, [ADR-0018](adr/0018-control-surface-generation.md) — but the first-class boundary declaration is deferred to the nesting/contract thread.)*

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

An Operator carries a self-describing **descriptor** (ports + rich metadata: range, default, units, response curve) separate from its process function. Each functional input is one **`Input`** described by a single axis, its `shape` — `Float` (a number), `Enum` (a named choice), `Harmony` (the tonal-context struct), or `Note` (an event) — with delivery and read-style following from the shape; instantiate-time configuration (e.g. voice count) is a separate `Constant` ([ADR-0028](adr/0028-one-input-shape.md)). The descriptor is the seat of "good button" (auto-generated controls that can't sound bad), of serialization, of shape-checking connections, and of AI grounding.

Operators are **single-Lane** by default ([ADR-0010](adr/0010-single-lane-operators.md)): the author writes one mono, single-Voice stream (a block at a time) and the engine fans it out across all **Lanes** (Voice × Channel) with per-Lane state. Cross-cutting work (voicing, mixing, panning) is preferentially expressed *above* the operator layer — the **Voicer**, deterministic fan-in connections, Channel-aware boundary constructs — with operator-level full-set access as a discouraged escape hatch.

**Voices** (independent sounding instances, from a pre-allocated pool bounded at Instantiate) are distinct from **Channels** (n-channel signal paths); a stereo Voice spans two Channels.

**Message delivery** ([ADR-0011](adr/0011-message-delivery-and-timing.md)) is sample-accurate but author-transparent: the engine **block-slices** at Message boundaries so a single-Lane author just reads "my current value" while a knob change lands at the exact sample. Event-oriented Operators (the Clock, the Voicer, the sequencer) instead receive the routed Messages as zero-copy `Event` views (address local to the node, args, segment-relative frame) via `Io::events`, because they reason in events. Operators also **emit** Messages onto wired Message edges ([ADR-0014](adr/0014-internal-message-graph.md)) — a sequencer feeding a Voicer — and a `context` Operator **publishes** a latched tonal-context struct read back via `Io::harmony` ([ADR-0015](adr/0015-latched-context-read.md), the `Harmony` shape), the struct-valued sibling of a knob.

## Musical layer

- **Clock** ([ADR-0006](adr/0006-clock-and-musical-time.md)): a global default Clock so everything grooves together out of the box; Clocks are also Operators for polytempo/generative timing. The Clock provides base timing only — groove, swing, and feel are separate Operators. Timetags default to musical time, resolved against the Clock at dispatch.
- **Pitch and tuning** ([ADR-0008](adr/0008-pitch-and-tuning.md)): symbolic Pitch (scale-degree primary, float-MIDI-note available) resolved to frequency by a **Tuning**. Tunings import from Scala `.scl`/`.kbm`, supporting any non-Western or user-defined system; 12-TET is just the default. The active Tuning rides the **tonal-context** bus alongside key/scale/chord, queried continuously, so retuning while notes sound works.
- **Tonal-context bus** ([ADR-0013](adr/0013-tonal-context-bus-mechanics.md), [ADR-0015](adr/0015-latched-context-read.md)): the key/scale/chord is a latched `Copy` struct a `context` Operator publishes and followers read via `Io::harmony` (the `Harmony` shape) — the resolver (`hz`/`snap`/`chord_tone`) lives in that one struct, so a follower stays dumb. Degree notes resolve to Hz through it (a held line **re-spells live** on a key/scale change), a `snap` Operator quantizes arbitrary pitch to the nearest in-scale degree, and changes are sample-accurate on the same timeline as notes. *(Built for 12-TET; the Scala-tuning swap rides the same step-space seam and is deferred.)*

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

The MVP is a headless "it makes a sound" spine: the portable-core / native-crate split, Signal + Message, the Plan + Instantiate→Render loop, single-Lane fan-out, determinism, a serial executor behind the real interface, the core Operators (oscillator, envelope, filter, Voicer, output, Clock), OSC-in from TouchOSC/Max, default 12-TET. Get past the prototype graveyard fast, then build the UX. V1.1 has since added music Operators (delay, reverb, LFO, sequencer, sample player), the internal message graph (operators emit Messages), the tonal-context bus (a `context` Operator + degree resolution + a `snap` Operator), and the resource store (decoded sample data as a shared, bank-ready read service). For the code-level operator contract and how to add one, see [docs/agents/authoring.md](agents/authoring.md).

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
- [0028 — One `Input`, one axis: `shape` (Float/Enum/Harmony/Note); density and delivery follow from it](adr/0028-one-input-shape.md)
