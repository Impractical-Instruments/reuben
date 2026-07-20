# reuben rules index

The now-state architecture, as rules. Read top-down and **stop at the shallowest level
that answers your question**:

    index (this file)  →  topic doc   →  a rule         →  its rationale
    summaries+glossary     now-story +    present-tense     condensed "why",
                           its rules      statement         read only when needed

- A **topic** is one area of the system: its "now" story plus the rules that hold there.
- A **rule** is a present-tense normative statement with a stable anchor and one rationale link.
- A **rationale** is the condensed "why", loaded only when needed. Provenance lives there.

Code points at topics, never at rules or ADRs: `// see rules: <topic>` (this repo),
`// see engine rules: <topic>` (web → engine). See [Conventions](#conventions).

## Topics

<!-- derived — collated from each topic's `> summary`; do not hand-edit out of sync. -->
- **[Authoring surface & instrument library](authoring-library.md)** — How authoring surfaces and the instrument library sit on top of the graph — decoupled surface docs over interface pipes, Good Buttons, the sample/resource store, library resolution and format versioning, and the launch Toys.
- **[Composition & operator model](composition-operators.md)** — The one recursive graph — how operators declare and register their contract, how all data flows as one Message/Arg substrate in Value, Event, and Signal forms, and how instruments nest and expose interface pipes.
- **[Execution & runtime](execution-runtime.md)** — How the unified block graph is scheduled, threaded, swapped, and rendered in real time — the Plan lifecycle, RT boundary, determinism, latch service, and the embed surface.
- **[Signal, OSC, musical time & DSP](signal-time-dsp.md)** — How signal and musical meaning are carried, timed, and shaped — the OSC-only Message model, the Clock and musical time, symbolic pitch and Tuning, the tonal-context bus, and the envelope/curve/math DSP families.

## Glossary

<!-- derived — collated from each topic's `## Terms`, linking the defining topic. -->
- **available-set** — the set of instruments a session can reference. · [authoring-library](authoring-library.md)
- **Arg** — the single closed-enum payload a Message carries: OSC primitives, shared vocab types (`Note`, `Harmony`), an erased enum index, or the dense `Buffer`. · [composition-operators](composition-operators.md)
- **Block** — the fixed-size processing quantum; each block computes message- and signal-domain data in one dependency-ordered pass. · [execution-runtime](execution-runtime.md)
- **Boundary adapter** — a removable I/O-edge component that converts a foreign protocol (MIDI, Ableton Link, external OSC) to and from the core's OSC-shaped Messages. · [signal-time-dsp](signal-time-dsp.md)
- **Clock** — the Operator providing base musical timing — tempo, meter, the beat grid — as a sample-accurate beat phasor; a default instance syncs a Rig. · [signal-time-dsp](signal-time-dsp.md)
- **Constant** — a plan-time immutable port whose value is fixed at instantiate; changing it rebuilds the graph. · [composition-operators](composition-operators.md)
- **Coordinator** — the single non-RT writer of graph structure; owns the canonical graph and instrument library and performs every Swap. · [execution-runtime](execution-runtime.md)
- **CV** — a linear control signal in a normalized range (e.g. an envelope's `[0, 1]` contour), carried untyped on the Signal domain and interpreted by downstream ops. · [signal-time-dsp](signal-time-dsp.md)
- **Embed surface** — the portable rim of reuben-core (the `Engine` bridge) that each host shell wraps; the native I/O layer is the removable other side. · [execution-runtime](execution-runtime.md)
- **Engine** — the portable bridge in reuben-core (`queue_osc` → `fill` → `drain_outbound`) a host shell drives, and the whole vessel (Plan + Renderer + scratch) that a Swap crosses. · [execution-runtime](execution-runtime.md)
- **format_version** — the document's integer shape marker; absent means 1, save writes the current version, and only a breaking shape change bumps it. · [authoring-library](authoring-library.md)
- **Event** — an unlatched, multi-valued, frame-stamped port form (`note`), read as a stream and never sliced. · [composition-operators](composition-operators.md)
- **frame** — a sample offset within a block; the unit of sample-accurate Message timing. · [execution-runtime](execution-runtime.md)
- **Groove** — a per-stream re-timing of a Message stream (swing/feel), applied by a separate Operator, distinct from the Clock's base grid. · [signal-time-dsp](signal-time-dsp.md)
- **Good Button** — a curated player-facing control that is hard to make sound bad, built from composition (a fan of `map`s) rather than from new instrument-format machinery. · [authoring-library](authoring-library.md)
- **Instantiate** — the off-thread construction of a Plan (topo sort, cluster, allocate the delta); the first half of every Swap, where all allocation lives. · [execution-runtime](execution-runtime.md)
- **Instrument** — a named subgraph that exposes an interface and is reused inside another graph as if it were an operator, with its own identity and state per use. · [composition-operators](composition-operators.md)
- **interface pipe** — a named boundary entry, the one boundary mechanism at every graph level: an input pipe mints an address, an output pipe is fed from an internal port. · [composition-operators](composition-operators.md)
- **latch** — the engine-held per-port zero-order-hold of an input's last Message, read by an operator as its constant current value. · [execution-runtime](execution-runtime.md)
- **logical channel** — the device-independent channel index a signal pipe binds; a device profile, not the patch, maps it to hardware. · [composition-operators](composition-operators.md)
- **Message** — the one data unit: `{ address, frame, Arg }`, carrying exactly one `Arg`. · [composition-operators](composition-operators.md)
- **Operator** — the smallest node: a unit of DSP behavior, authored as one single-voice, single-channel block-at-a-time stream that the engine schedules. · [composition-operators](composition-operators.md)
- **library index** — the generated one-signature-line-per-instrument projection of the available-set (name + recipe-role + interface face). · [authoring-library](authoring-library.md)
- **NormalizedDoc** — the type minted exactly once at the parse gate (refuse the future, migrate the past, strip retired presentation, stamp) that every build and load path accepts, proving a document is current-shaped and migrated exactly once. · [authoring-library](authoring-library.md)
- **Plan** — the runtime artifact: the immutable, already-allocated static parallel schedule (topo-ordered, clustered) that Render executes per block. · [execution-runtime](execution-runtime.md)
- **recipe-role** — an instrument's reuse story: the first sentence of its `doc` field, trusted for selection only, never for wiring. · [authoring-library](authoring-library.md)
- **Render** — the hard-realtime, allocation-free per-block execution of the current Plan on the audio thread. · [execution-runtime](execution-runtime.md)
- **ResourceStore** — the central store of decoded resource bytes, built by the Coordinator at load and read immutably by Render through one pure `(id, range)` accessor, keyed by logical id. · [authoring-library](authoring-library.md)
- **surface doc** — the presentation-only document that binds an instrument's interface input-pipe names to widgets, decoupled from the instrument itself. · [authoring-library](authoring-library.md)
- **Rig** — the outermost graph, the one actually played at top level. · [composition-operators](composition-operators.md)
- **Scale** — ordered step-offsets within a Tuning's period plus a root, mapping a scale degree to a step index (symbolic → symbolic). · [signal-time-dsp](signal-time-dsp.md)
- **Signal** — a Message whose `Arg` is a `Buffer`; the dense, per-sample port form (`f32_buffer`). · [composition-operators](composition-operators.md)
- **Snap** — quantizing an arbitrary pitch to the nearest in-scale degree under a caller-supplied policy, upstream of resolution. · [signal-time-dsp](signal-time-dsp.md)
- **subpatch** — a node referencing a nested instrument, inlined and dissolved into the parent graph at build. · [composition-operators](composition-operators.md)
- **survivor** — an operator that persists across a Swap (matched on address + type + instantiate-time fingerprint) and keeps its state via box transplant. · [execution-runtime](execution-runtime.md)
- **Swap** — the single off-thread transition that installs a new Plan/Engine at a block boundary, migrating survivor state and reclaiming the old vessel. · [execution-runtime](execution-runtime.md)
- **Toy** — a launch beginner instrument assembled from existing operators plus a generated surface, one per distinct player gesture. · [authoring-library](authoring-library.md)
- **Tonal context** — the latched key/scale/chord/tuning value, owned by a context Operator, that followers resolve pitch against. · [signal-time-dsp](signal-time-dsp.md)
- **Tuning** — the resolution layer mapping a symbolic pitch (a scale step) to a frequency in Hz; 12-TET is the default, Scala-importable. · [signal-time-dsp](signal-time-dsp.md)
- **Value** — a latched, held, single-valued port form (`f32`/`enum`/`harmony`/`i32`), read as a constant within a `process` call via zero-order-hold. · [composition-operators](composition-operators.md)

## Conventions

**Layout**

```
docs/rules/README.md                     index: topic summaries + derived glossary
docs/rules/<topic>.md                    now-story + rules; each rule links its rationale
docs/rules/rationale/<topic>/<rule>.md   condensed why + "Distilled from: ADR-00xx"
docs/adr/                                live ADRs (iteration surface); see docs/adr/README.md
```

**Rule** — a present-tense normative statement, one sentence. Carries a stable kebab-case
slug (unique within its topic) as a raw-HTML `<a id>` anchor above the heading, so the
sentence can be reworded without breaking links. Exactly one rationale link.

**Rationale** — the condensed "why" that still applies; superseded/dead-end paths are dropped
(git keeps them). Ends with a `Distilled from: ADR-00xx[, ADR-00yy]` provenance line. One file
per rule at `rationale/<topic>/<rule>.md`.

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
