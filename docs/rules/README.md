# reuben rules index

**reuben is a configurable musical instrument.** You build behavior by patching **Operators** —
small units that each do one simple thing — into **Instruments**, and Instruments into a **Rig** (a
full playable system). Beginners start with **Toys**: ready-made instruments that play instantly.
The same engine that makes music can drive lights, video, or a game engine, because the data flowing
through it is general.

This file is the **front door** to how reuben works — the single index that absorbed the old
end-to-end design narrative (`ARCHITECTURE.md`) and the glossary (`CONTEXT.md`). It is the now-state
architecture, as rules. Read top-down and **stop at the shallowest level that answers your
question**:

    index (this file)  →  topic doc   →  a rule         →  its rationale
    summaries+glossary     now-story +    present-tense     condensed "why",
                           its rules      statement         read only when needed

- A **topic** is one area of the system: its "now" story plus the rules that hold there.
- A **rule** is a present-tense normative statement with a stable anchor and one rationale link.
- A **rationale** is the condensed "why", loaded only when needed. Provenance lives there.

Code points at topics, never at rules or ADRs: `// see rules: <topic>` (this repo),
`// see engine rules: <topic>` (web → engine). See [Conventions](#conventions).

> **These rules describe the now-state design.** For what is actually built and running today, see
> the [README status line](../../README.md). Where a designed mechanism is not built yet, its topic
> doc flags it inline.

## Design ethos

A few load-bearing commitments run under every topic; each is held by mechanism in the topic docs,
not by hope:

- **Good button.** Every control is hard to make sound bad — energy in produces musical feedback
  out, and easy defaults always exist ([authoring surface & instrument library](authoring-library.md)).
- **Easy to learn, deep to master.** Toys and defaults on the surface; recursive composition and
  full control underneath — a default Clock you can override, a default Tuning you can replace, a
  curated playable surface over structural addresses ([composition & operator model](composition-operators.md),
  [signal, OSC, musical time & DSP](signal-time-dsp.md)).
- **AI-authorable, first-class.** Agents read the system and author Operators, Instruments, and Rigs
  against a self-describing, one-recursive-model JSON format ([agent framework & MCP](agent-mcp.md)).
- **OSC is the lingua franca.** Internal Messages and external OSC are the same idea; every other
  protocol converts at the boundary ([signal, OSC, musical time & DSP](signal-time-dsp.md)).
- **Portable core, removable shells.** The realtime core is OS-free Rust wrapped by thin
  per-platform shells at one embed surface ([execution & runtime](execution-runtime.md),
  [web/product boundary & dev process](web-product-process.md)).

## Topics

<!-- derived — collated from each topic's `> summary`; do not hand-edit out of sync. -->
- **[Agent framework & MCP](agent-mcp.md)** — How AI agents author reuben — authorability as a first-class constraint, the introspect/validate loop, the authoring skills, and the MCP sidecar whose tool contracts are one OS-free source behind every door.
- **[Authoring surface & instrument library](authoring-library.md)** — How authoring surfaces and the instrument library sit on top of the graph — decoupled surface docs over interface pipes, Good Buttons, the sample/resource store, library resolution and format versioning, and the launch Toys.
- **[Composition & operator model](composition-operators.md)** — The one recursive graph — how operators declare and register their contract, how all data flows as one Message/Arg substrate in Value, Event, and Signal forms, and how instruments nest and expose interface pipes.
- **[Execution & runtime](execution-runtime.md)** — How the unified block graph is scheduled, threaded, swapped, and rendered in real time — the Plan lifecycle, RT boundary, determinism, latch service, and the embed surface.
- **[Signal, OSC, musical time & DSP](signal-time-dsp.md)** — How signal and musical meaning are carried, timed, and shaped — the OSC-only Message model, the Clock and musical time, symbolic pitch and Tuning, the tonal-context bus, and the envelope/curve/math DSP families.
- **[Web/product boundary & dev process](web-product-process.md)** — How this repo sits under the web/product boundary: the BSD SDK a private product consumes, the raw C-ABI browser contract and sample-trust obligation it owes, and the branch, release, toolchain, and perf-benchmark process that governs it.

## Glossary

<!-- derived — collated from each topic's `## Terms`, linking the defining topic. -->
- **Arg** — the single closed-enum payload a Message carries: OSC primitives, shared vocab types (`Note`, `Harmony`), an erased enum index, or the dense `Buffer`. · [composition-operators](composition-operators.md)
- **available-set** — the set of instruments a session can reference. · [authoring-library](authoring-library.md)
- **Block** — the fixed-size processing quantum; each block computes message- and signal-domain data in one dependency-ordered pass. · [execution-runtime](execution-runtime.md)
- **Boundary adapter** — a removable I/O-edge component that converts a foreign protocol (MIDI, Ableton Link, external OSC) to and from the core's OSC-shaped Messages. · [signal-time-dsp](signal-time-dsp.md)
- **C-ABI worklet boundary** — the documented raw `extern "C"`, `(ptr, len)`-over-linear-memory interface a browser host drives per audio quantum, carrying no `wasm-bindgen` glue and shipped as a contract to rebuild against, not a maintained binding. · [web-product-process](web-product-process.md)
- **Clock** — the Operator providing base musical timing — tempo, meter, the beat grid — as a sample-accurate beat phasor; a default instance syncs a Rig. · [signal-time-dsp](signal-time-dsp.md)
- **Constant** — a plan-time immutable port whose value is fixed at instantiate; changing it rebuilds the graph. · [composition-operators](composition-operators.md)
- **Coordinator** — the single non-RT writer of graph structure; owns the canonical graph and instrument library and performs every Swap. · [execution-runtime](execution-runtime.md)
- **CV** — a linear control signal in a normalized range (e.g. an envelope's `[0, 1]` contour), carried untyped on the Signal domain and interpreted by downstream ops. · [signal-time-dsp](signal-time-dsp.md)
- **Delivery lane** — a grounding consumer (repo skills, MCP clients, web chat), each reducing to transport bindings plus host furniture plus the shared base sauce, fed by push or pull. · [agent-mcp](agent-mcp.md)
- **Door** — one surface over the OS-free contract types (native CLI, MCP sidecar, web in-page layer, web proxy); no verb means different things behind different doors. · [agent-mcp](agent-mcp.md)
- **Embed surface** — the portable rim of reuben-core (the `Engine` bridge) that each host shell wraps; the native I/O layer is the removable other side. · [execution-runtime](execution-runtime.md)
- **Engine** — the portable bridge in reuben-core (`queue_osc` → `fill` → `drain_outbound`) a host shell drives, and the whole vessel (Plan + Renderer + scratch) that a Swap crosses. · [execution-runtime](execution-runtime.md)
- **Event** — an unlatched, multi-valued, frame-stamped port form (`note`), read as a stream and never sliced. · [composition-operators](composition-operators.md)
- **format_version** — the document's integer shape marker; absent means 1, save writes the current version, and only a breaking shape change bumps it. · [authoring-library](authoring-library.md)
- **frame** — a sample offset within a block; the unit of sample-accurate Message timing. · [execution-runtime](execution-runtime.md)
- **Gist-and-point** — the anti-drift posture for prose that must live in code: carry the one-breath gist and point at the single canonical doc, never restate it. · [agent-mcp](agent-mcp.md)
- **Good Button** — a curated player-facing control that is hard to make sound bad, built from composition (a fan of `map`s) rather than from new instrument-format machinery. · [authoring-library](authoring-library.md)
- **Groove** — a per-stream re-timing of a Message stream (swing/feel), applied by a separate Operator, distinct from the Clock's base grid. · [signal-time-dsp](signal-time-dsp.md)
- **Input handling** — interpreting musical, mood, or abstract language as patching moves; the shared base grounding identical in every lane. · [agent-mcp](agent-mcp.md)
- **Instantiate** — the off-thread construction of a Plan (topo sort, cluster, allocate the delta); the first half of every Swap, where all allocation lives. · [execution-runtime](execution-runtime.md)
- **Instrument** — a named subgraph that exposes an interface and is reused inside another graph as if it were an operator, with its own identity and state per use. · [composition-operators](composition-operators.md)
- **Intent vocabulary** — the one curated, registry-keyed word→move table that grounds musical/mood words (warmer, busier, sadder) as operator-type parameter moves. · [agent-mcp](agent-mcp.md)
- **interface pipe** — a named boundary entry, the one boundary mechanism at every graph level: an input pipe mints an address, an output pipe is fed from an internal port. · [composition-operators](composition-operators.md)
- **latch** — the engine-held per-port zero-order-hold of an input's last Message, read by an operator as its constant current value. · [execution-runtime](execution-runtime.md)
- **library index** — the generated one-signature-line-per-instrument projection of the available-set (name + recipe-role + interface face). · [authoring-library](authoring-library.md)
- **logical channel** — the device-independent channel index a signal pipe binds; a device profile, not the patch, maps it to hardware. · [composition-operators](composition-operators.md)
- **Message** — the one data unit: `{ address, frame, Arg }`, carrying exactly one `Arg`. · [composition-operators](composition-operators.md)
- **NormalizedDoc** — the type minted exactly once at the parse gate (refuse the future, migrate the past, strip retired presentation, stamp) that every build and load path accepts, proving a document is current-shaped and migrated exactly once. · [authoring-library](authoring-library.md)
- **Operator** — the smallest node: a unit of DSP behavior, authored as one single-voice, single-channel block-at-a-time stream that the engine schedules. · [composition-operators](composition-operators.md)
- **Output filter** — the host-owned persona: what the person is shown (sound-not-machine subject, hidden diagnostics, register), maximal on web and absent at skills/MCP. · [agent-mcp](agent-mcp.md)
- **perf gate** — the CI iai-callgrind instruction-count check that fails a PR on a >10% regression of the render hot path, base-ref-relative so toolchain drift cancels. · [web-product-process](web-product-process.md)
- **Plan** — the runtime artifact: the immutable, already-allocated static parallel schedule (topo-ordered, clustered) that Render executes per block. · [execution-runtime](execution-runtime.md)
- **product repo** — the separate private AGPL repo holding the browser shell, player app, share-link codec, and chat-authoring agent, which pins this repo as a submodule. · [web-product-process](web-product-process.md)
- **promotion** — the fast-forward-only advance of `dev` onto `main` that ships production, run as a workflow so commit SHAs are preserved and the branches never diverge. · [web-product-process](web-product-process.md)
- **recipe-role** — an instrument's reuse story: the first sentence of its `doc` field, trusted for selection only, never for wiring. · [authoring-library](authoring-library.md)
- **Render** — the hard-realtime, allocation-free per-block execution of the current Plan on the audio thread. · [execution-runtime](execution-runtime.md)
- **ResourceStore** — the central store of decoded resource bytes, built by the Coordinator at load and read immutably by Render through one pure `(id, range)` accessor, keyed by logical id. · [authoring-library](authoring-library.md)
- **Rig** — the outermost graph, the one actually played at top level. · [composition-operators](composition-operators.md)
- **Scale** — ordered step-offsets within a Tuning's period plus a root, mapping a scale degree to a step index (symbolic → symbolic). · [signal-time-dsp](signal-time-dsp.md)
- **SDK** — this (BSD-3-Clause) repo: the engine core, native CLI, MCP sidecar, and instrument/surface library that the product consumes. · [web-product-process](web-product-process.md)
- **share link** — an origin-independent encoded bundle that boots an instrument in the browser; a product-repo feature whose residue here is the sample-bytes trust obligation. · [web-product-process](web-product-process.md)
- **Sidecar** — the disposable per-conversation MCP stdio process the client spawns: pure tools in-process, engine tools forwarded to the user-owned engine. · [agent-mcp](agent-mcp.md)
- **Signal** — a Message whose `Arg` is a `Buffer`; the dense, per-sample port form (`f32_buffer`). · [composition-operators](composition-operators.md)
- **Snap** — quantizing an arbitrary pitch to the nearest in-scale degree under a caller-supplied policy, upstream of resolution. · [signal-time-dsp](signal-time-dsp.md)
- **subpatch** — a node referencing a nested instrument, inlined and dissolved into the parent graph at build. · [composition-operators](composition-operators.md)
- **surface doc** — the presentation-only document that binds an instrument's interface input-pipe names to widgets, decoupled from the instrument itself. · [authoring-library](authoring-library.md)
- **survivor** — an operator that persists across a Swap (matched on address + type + instantiate-time fingerprint) and keeps its state via box transplant. · [execution-runtime](execution-runtime.md)
- **Swap** — the single off-thread transition that installs a new Plan/Engine at a block boundary, migrating survivor state and reclaiming the old vessel. · [execution-runtime](execution-runtime.md)
- **Tonal context** — the latched key/scale/chord/tuning value, owned by a context Operator, that followers resolve pitch against. · [signal-time-dsp](signal-time-dsp.md)
- **toolchain pin** — the exact-version `rust-toolchain.toml` that local dev and CI share so their fmt/clippy verdicts are identical, kept in lockstep with the workspace MSRV. · [web-product-process](web-product-process.md)
- **Toy** — a launch beginner instrument assembled from existing operators plus a generated surface, one per distinct player gesture. · [authoring-library](authoring-library.md)
- **Tuning** — the resolution layer mapping a symbolic pitch (a scale step) to a frequency in Hz; 12-TET is the default, Scala-importable. · [signal-time-dsp](signal-time-dsp.md)
- **Value** — a latched, held, single-valued port form (`f32`/`enum`/`harmony`/`i32`), read as a constant within a `process` call via zero-order-hold. · [composition-operators](composition-operators.md)

## Avoid these synonyms

<!-- HAND-AUTHORED — preserved verbatim from the retired CONTEXT.md. NOT derived: the collated
     Glossary above holds one canonical term per topic, but this synonym guidance covers more terms
     than the topic `## Terms` expose, so it lives here in full. The derive script leaves this
     section alone (it only rewrites `## Topics` and `## Glossary`). -->

Each domain term has one canonical spelling. These are the near-misses to keep out of code, issues,
and prose:

- **Operator** — avoid: node, object, module, block (block = an audio buffer chunk), ugen, plugin.
- **Instrument** — avoid: patch (noun — see Patch), device, rack, module.
- **Rig** — avoid: project, set, session, scene, song.
- **Patch** (verb) — avoid: patch as a noun.
- **Toy** — avoid: preset, template.
- **Address** — avoid: path, route, id (id = internal identity, not the address).
- **Coordinator** — avoid: engine, manager, host in the system-embedder sense (a host application embeds the system; the Coordinator owns the graph — the Voicer's host path is a different, sanctioned sense).
- **Plan** — avoid: schedule, graph image, compiled graph.
- **Swap** — avoid: hot-swap (describes how, not the phase), re-plan, recompile, reload.
- **Survivor** — avoid: carried node, kept node, matched node.
- **Restart-swap** — avoid: reload, hot restart.
- **Structure channel** — avoid: control channel (that is OSC), admin port, command socket.
- **Gist-and-point** — avoid: duplicate-then-sync (the sweep is a backstop, not the mechanism), summary copy.
- **Render** — avoid: block time, process, audio callback (the callback is the host of Render, not Render itself).
- **Lane** _(retired)_ — don't use it for new work; say Voice, Channel, or Voice instrument. It survives only in frozen ADRs.
- **Voice** — avoid: channel, note (a note is a Message; a Voice is what sounds it).
- **Channel** — avoid: voice, bus.
- **Voicer** — avoid: allocator, poly, note manager.
- **Voice instrument** — avoid: voice sub-patch (retired — role, not kind), voice graph, sub-instrument, voice template.
- **Interface** — avoid: control surface, ports block.
- **Interface makes the role** — avoid: recipe as a kind of document, role-by-directory, naming conventions for role.
- **Subpatch** — avoid: subpatch as a noun for the document, sub-instrument, nested patch, embedded instrument.
- **Inline (dissolve)** — avoid: expand, flatten (as the term of art), instantiate.
- **Host** — avoid: runtime nest, sub-plan path (informal).
- **Boundary face** — avoid: descriptor (the compile-time operator contract), synthesized ports (informal).
- **Surface doc** — avoid: control block (the retired inline per-node form), layout file, UI config, `.tosc` (a projection of the doc, not the doc).
- **Superset widget vocabulary** — avoid: widget list, control types, per-target vocabulary (the vocabulary is shared; only rendering is per-target).
- **Surface pipe promotion** — avoid: exposing a param (informal — say promotion), control migration, lane pipe (shelved future sugar, not this).
- **Pitch** — avoid: note number (alone), frequency (frequency is the resolved result, not the Pitch).
- **Tuning** — avoid: temperament, scale (scale = which degrees are in play; Tuning = their frequencies).
- **Scale** — avoid: mode (a mode is one kind of Scale), key (key is part of the Scale).
- **Harmony** — avoid: tonal context, context, harmony bus, key signature.
- **Clock** — avoid: transport, master clock, conductor.
- **Good Button** — avoid: meta param, meta-control, macro (all name the artifact — say Good Button).
- **Signal** — avoid: CV, audio buffer / control buffer (as distinct types), wire, carrier, read-view of a Float.
- **Value** — avoid: param, scalar, control (as a distinct type), Float.
- **Event** — avoid: trigger, stream (as a type), notes (plural, as a type).
- **Buffer** — avoid: arena, sample array, f32 slice (as the domain term).
- **Message** — avoid: event, control, OSC packet (as a distinct internal type), typed args (plural — a Message holds exactly one Arg).
- **Input** — avoid: port, param, connection, slot (the slot is the Input; its payload is the Arg).
- **Handle** — avoid: port handle, index const (the handle replaced the bare `usize` const), port.
- **Arg** — avoid: shape, kind, PortKind, value, blob, carrier, port.
- **vocab** — avoid: enum registry, type table, concrete-arg module.
- **Held value (ZOH latch)** — avoid: context, param latch, enum latch (as separate mechanisms), state.
- **Constant** — avoid: param, setting, option, config value.
- **Delivery lane** — avoid: surface (that is a presentation doc), channel (that is signal I/O), bare "lane" without context.
- **Input handling** — avoid: intent parsing, NLU.
- **Output filter** — avoid: persona (ambiguous), style gate (deleted — the filter is taught, not enforced).
- **Push/pull delivery** — avoid: eager/lazy loading (runtime words for a prompt-architecture idea).

## Conventions

**Layout**

```
docs/rules/README.md                     index: topic summaries + derived glossary
docs/rules/<topic>.md                    now-story + rules; each rule links its rationale
docs/rules/rationale/<topic>/<rule>.md   condensed why + "Distilled from: ADR-NNNN"
docs/adr/                                live ADRs (iteration surface); see docs/adr/README.md
```

**Rule** — a present-tense normative statement, one sentence. Carries a stable kebab-case
slug (unique within its topic) as a raw-HTML `<a id>` anchor above the heading, so the
sentence can be reworded without breaking links. Exactly one rationale link.

**Rationale** — the condensed "why" that still applies; superseded/dead-end paths are dropped
(git keeps them). Ends with a `Distilled from: ADR-NNNN[, ADR-MMMM]` provenance line — or, for a
rule settled without ever passing through an ADR, `Decided in: issue #NNN — settled directly, no
ADR.` (a rule is allowed to be born here; do not back-fill an ADR just to satisfy the template).
One file per rule at `rationale/<topic>/<rule>.md`.

**Code-comment reference** — topic-level only, never a rule slug or ADR number:
`// see rules: <topic>` in-repo, `// see engine rules: <topic>` cross-repo. Grammar:
`/\bsee (engine )?rules: ([a-z0-9-]+)/`; the slug must resolve to a topic doc.

**Progressive-disclosure ladder** — index → topic → rule → rationale. Stop at the shallowest
level that answers the question; open a rationale only when you need the why.

**Derived index** — the Topics list and Glossary above are collated from the topic docs; do not
hand-edit them. The `pre-commit` hook regenerates them (`check_rules_derive.py --write`) whenever a
commit touches `docs/rules/`, and CI runs `--check` as a backstop. Run `scripts/install-hooks.sh`
once per clone.

**ADR lifecycle** — see [docs/adr/README.md](../adr/README.md).
