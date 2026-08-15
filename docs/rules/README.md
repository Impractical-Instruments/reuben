# reuben rules index

**reuben is a configurable musical instrument.** You build behavior by patching **Operators** —
small units that each do one simple thing — into **Instruments**, and Instruments into a **Rig** (a
full playable system). Beginners start with **Toys**: ready-made instruments that play instantly.
The same engine that makes music can drive lights, video, or a game engine, because the data flowing
through it is general.

This file is the **front door** to how reuben works — the now-state architecture, as rules. Read
top-down and **stop at the shallowest level that answers your question**:

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
- **[Code as a grounding surface](code-as-grounding.md)** — How this repo's own source text is governed as grounding an agent reads — comment discipline that points at rules instead of restating them, LSP-first navigation, and pre-scoped search.
- **[Composition & operator model](composition-operators.md)** — The one recursive graph — how operators declare and register their contract, how all data flows as one Message/Arg substrate in Value, Event, and Signal forms, and how instruments nest and expose interface pipes.
- **[Execution & runtime](execution-runtime.md)** — How the unified block graph is scheduled, threaded, swapped, and rendered in real time — the Plan lifecycle, RT boundary, determinism, latch service, and the embed surface.
- **[Host shell & native I/O](host-shell-io.md)** — What a host shell owes the engine at the edges it owns — devices and their foreign clocks, the resampling and drift compensation that reconcile them, the latency that buys, and the fixed, counted way every edge degrades.
- **[Signal, OSC, musical time & DSP](signal-time-dsp.md)** — How signal and musical meaning are carried, timed, and shaped — the OSC-only Message model, the Clock and musical time, symbolic pitch and Tuning, the tonal-context bus, and the envelope/curve/math DSP families.
- **[Web/product boundary & dev process](web-product-process.md)** — How this repo sits under the web/product boundary: the BSD SDK a private product consumes, the raw C-ABI browser contract and sample-trust obligation it owes, and the release, toolchain, and perf-benchmark process that governs it.

## Glossary

<!-- derived — collated from each topic's `## Terms`, linking the defining topic. -->
- **Advertised description** — prose a door hands a model over the wire (a tool or schema `description`, the server `instructions`); generated from a doc comment, governed as model-facing prose. · [code-as-grounding](code-as-grounding.md)
- **Arg** — the single closed-enum payload a Message carries: OSC primitives, shared vocab types (`Note`, `Harmony`), an erased enum index, or the dense `Buffer`. · [composition-operators](composition-operators.md)
- **available-set** — the set of instruments a session can reference. · [authoring-library](authoring-library.md)
- **Block** — the fixed-size processing quantum; each block computes message- and signal-domain data in one dependency-ordered pass. · [execution-runtime](execution-runtime.md)
- **Boundary adapter** — a removable I/O-edge component that converts a foreign protocol (MIDI, Ableton Link, external OSC) to and from the core's OSC-shaped Messages. · [signal-time-dsp](signal-time-dsp.md)
- **C-ABI worklet boundary** — the documented raw `extern "C"`, `(ptr, len)`-over-linear-memory interface a browser host drives per audio quantum, carrying no `wasm-bindgen` glue and shipped as a contract to rebuild against, not a maintained binding. · [web-product-process](web-product-process.md)
- **Clock** — the Operator providing base musical timing — tempo and the beat grid — as a sample-accurate beat phasor; a default instance syncs a Rig. · [signal-time-dsp](signal-time-dsp.md)
- **Constant** — a plan-time immutable port whose value is fixed at instantiate; changing it rebuilds the graph. · [composition-operators](composition-operators.md)
- **Coordinator** — the single non-RT writer of graph structure; owns the canonical graph and instrument library and performs every Swap. · [execution-runtime](execution-runtime.md)
- **CV** — a linear control signal in a normalized range (e.g. an envelope's `[0, 1]` contour), carried untyped on the Signal domain and interpreted by downstream ops. · [signal-time-dsp](signal-time-dsp.md)
- **Dark degrade** — a shell edge's fixed response to a reality mismatch: play defined silence, then count it if it can recur or warn once if it cannot; never fail and never improvise. · [host-shell-io](host-shell-io.md)
- **Delivery lane** — a grounding consumer (repo skills, MCP clients, web chat), each reducing to transport bindings plus host furniture plus the shared base sauce, fed by push or pull. · [agent-mcp](agent-mcp.md)
- **Document verb** — one member of the closed, format-derived vocabulary an agent authors with: a stateless `(source, …)` mutator that applies one surgical edit, re-validates the whole document, and writes iff valid. · [agent-mcp](agent-mcp.md)
- **Door** — one surface over the window's contract types (native CLI, MCP sidecar, web in-page layer, web proxy); no verb means different things behind different doors. · [agent-mcp](agent-mcp.md)
- **Drift servo** — the control loop steering the input resample ratio to hold the ring's post-drain residual at a fixed floor, so the loop is independent of the host's variable callback size. · [host-shell-io](host-shell-io.md)
- **Embed surface** — the portable rim of reuben-core (the `Engine` bridge) that each host shell wraps; the native I/O layer is the removable other side. · [execution-runtime](execution-runtime.md)
- **Engine** — the portable bridge in reuben-core (`queue_osc` → `fill` → `drain_outbound`) a host shell drives, and the whole vessel (Plan + Renderer + scratch) that a Swap crosses. · [execution-runtime](execution-runtime.md)
- **EngineHost** — the seam a host serving the structure channel fills with what only a host can know: path resolution, control ingress, the counters, the device-map republish after a Swap, and the gate the deferred free waits on. · [execution-runtime](execution-runtime.md)
- **Event** — an unlatched, multi-valued, frame-stamped port form (`note`), read as a stream and never sliced. · [composition-operators](composition-operators.md)
- **Foreign edge** — the only place OSC-the-binary-protocol appears: external controllers arriving at `reuben play` and `osc_out` nodes leaving it; every internal door frames the same flat `{address, args}` form its own way. · [signal-time-dsp](signal-time-dsp.md)
- **format_version** — the document's integer shape marker; absent means 1, save writes the current version, and only a breaking shape change bumps it. · [authoring-library](authoring-library.md)
- **frame** — a sample offset within a block; the unit of sample-accurate Message timing. · [execution-runtime](execution-runtime.md)
- **Gist-and-point** — the anti-drift posture for prose that must live in code: carry the one-breath gist and point at the single canonical doc, never restate it. · [agent-mcp](agent-mcp.md)
- **Good Button** — a curated player-facing control that is hard to make sound bad, built from composition (a fan of `map`s) rather than from new instrument-format machinery. · [authoring-library](authoring-library.md)
- **Groove** — a per-stream re-timing of a Message stream (swing/feel), applied by a separate Operator, distinct from the Clock's base grid. · [signal-time-dsp](signal-time-dsp.md)
- **Harmony** — the `Arg` leaf carrying a tonal-context value on a wire; the latched value itself is the Tonal context. · [signal-time-dsp](signal-time-dsp.md)
- **Host shell** — the removable per-platform layer wrapping the embed surface; it owns devices, foreign protocols, and the callback that hosts Render. · [host-shell-io](host-shell-io.md)
- **Input handling** — interpreting musical, mood, or abstract language as patching moves; the shared base grounding identical in every lane. · [agent-mcp](agent-mcp.md)
- **Instantiate** — the off-thread construction of a Plan (topo sort, allocate the delta); the first half of every Swap, where all allocation lives. · [execution-runtime](execution-runtime.md)
- **Instrument** — a named subgraph that exposes an interface and is reused inside another graph as if it were an operator, with its own identity and state per use. · [composition-operators](composition-operators.md)
- **Intent vocabulary** — the one curated, registry-keyed word→move table that grounds musical/mood words (warmer, busier, sadder) as operator-type parameter moves. · [agent-mcp](agent-mcp.md)
- **Intent word** — one word of that table, applied as the whole ordered batch of value edits its row names: the engine does the word→ports→arithmetic join, not the model. · [agent-mcp](agent-mcp.md)
- **interface pipe** — a named boundary entry, the one boundary mechanism at every graph level: an input pipe mints an address, an output pipe is fed from an internal port. · [composition-operators](composition-operators.md)
- **latch** — the engine-held per-port zero-order-hold of an input's last Message, read by an operator as its constant current value. · [execution-runtime](execution-runtime.md)
- **library index** — the generated one-signature-line-per-instrument projection of the available-set (name + recipe-role + interface face). · [authoring-library](authoring-library.md)
- **logical channel** — the device-independent channel index a signal pipe binds; a device profile, not the patch, maps it to hardware. · [composition-operators](composition-operators.md)
- **Mechanics** — the one kind of prose a comment may carry: the local, non-obvious fact the code cannot state itself (a `SAFETY:` justification, a caller-upheld invariant, why a constant is this number). · [code-as-grounding](code-as-grounding.md)
- **Message** — the one data unit: `{ address, frame, Arg }`, carrying exactly one `Arg`. · [composition-operators](composition-operators.md)
- **NormalizedDoc** — the type minted once at the parse gate that every build and load path accepts, proving a document is current-shaped and migrated exactly once. · [authoring-library](authoring-library.md)
- **Operator** — the smallest node: a unit of DSP behavior, authored as one single-voice, single-channel block-at-a-time stream that the engine schedules. · [composition-operators](composition-operators.md)
- **Output filter** — the host-owned persona: what the person is shown (sound-not-machine subject, hidden diagnostics, register), maximal on web and absent at skills/MCP. · [agent-mcp](agent-mcp.md)
- **perf gate** — the CI iai-callgrind instruction-count check over the render hot path, measured base-ref-relative so toolchain drift cancels. ADR-0077 adds a construct layer and, on it, a growth-factor check that reads no baseline. · [web-product-process](web-product-process.md)
- **Pitch** — a symbolic scale degree or an absolute 12-TET coordinate, carried as one enum case; the resolved Hz is the result, not the Pitch. · [signal-time-dsp](signal-time-dsp.md)
- **Plan** — the runtime artifact: the immutable, already-allocated, topologically ordered schedule that Render executes per block. · [execution-runtime](execution-runtime.md)
- **product repo** — the separate private AGPL repo holding the browser shell, player app, share-link codec, and chat-authoring agent, which pins this repo as a submodule. · [web-product-process](web-product-process.md)
- **recipe-role** — an instrument's reuse story: the first sentence of its `doc` field, trusted for selection only, never for wiring. · [authoring-library](authoring-library.md)
- **Render** — the hard-realtime, allocation-free per-block execution of the current Plan on the audio thread. · [execution-runtime](execution-runtime.md)
- **resource seam** — the one call *in*: the host-implemented trait through which the engine resolves a document's samples and nested children from opaque sources. · [authoring-library](authoring-library.md)
- **ResourceStore** — the central store of decoded resource bytes, built by the Coordinator at load and read immutably by Render through one pure `(id, channel, frame)` accessor, keyed by logical id. · [authoring-library](authoring-library.md)
- **Restatement** — comment prose that re-describes the code or answers what LSP or a search would answer; the third comment kind, deleted outright rather than moved to a rule. · [code-as-grounding](code-as-grounding.md)
- **Rig** — the outermost graph, the one actually played at top level. · [composition-operators](composition-operators.md)
- **Scale** — ordered step-offsets within a Tuning's period plus a root, mapping a scale degree to a step index (symbolic → symbolic). · [signal-time-dsp](signal-time-dsp.md)
- **SDK** — this (BSD-3-Clause) repo: the engine core, native CLI, MCP sidecar, and instrument/surface library that the product consumes. · [web-product-process](web-product-process.md)
- **share link** — an origin-independent encoded bundle that boots an instrument in the browser; a product-repo feature whose residue here is the sample-bytes trust obligation. · [web-product-process](web-product-process.md)
- **Sidecar** — the disposable per-conversation MCP stdio process the client spawns: pure tools in-process, engine tools forwarded to the user-owned engine. · [agent-mcp](agent-mcp.md)
- **Signal** — a Message whose `Arg` is a `Buffer`; the dense, per-sample port form (`f32_buffer`). · [composition-operators](composition-operators.md)
- **Snap** — quantizing an arbitrary pitch to the nearest in-scale degree under a caller-supplied policy, upstream of resolution. · [signal-time-dsp](signal-time-dsp.md)
- **Structural projection** — the agent's whole view of a document: partial per view (index, node zoom with reverse edges, pipes, resources), lossless only in aggregate, and single-sourced in reuben-core. · [agent-mcp](agent-mcp.md)
- **subpatch** — a node referencing a nested instrument, inlined and dissolved into the parent graph at build. · [composition-operators](composition-operators.md)
- **surface doc** — the presentation-only document that binds an instrument's interface input-pipe names to widgets, decoupled from the instrument itself. · [authoring-library](authoring-library.md)
- **survivor** — an operator that persists across a Swap (matched on address + type + instantiate-time fingerprint) and keeps its state via box transplant. · [execution-runtime](execution-runtime.md)
- **Swap** — the off-thread transition that installs a new Plan/Engine at a block boundary, migrating survivor state and reclaiming the old vessel; the whole-Engine unit until ADR-0076 lands the sub-Plan one. · [execution-runtime](execution-runtime.md)
- **Tonal context** — the latched key/scale/chord/tuning value, owned by a context Operator, that followers resolve pitch against. · [signal-time-dsp](signal-time-dsp.md)
- **toolchain pin** — the exact-version `rust-toolchain.toml` that local dev and CI share so their fmt/clippy verdicts are identical, kept in lockstep with the workspace MSRV. · [web-product-process](web-product-process.md)
- **Toy** — a launch beginner instrument assembled from existing operators plus a generated surface, one per distinct player gesture. · [authoring-library](authoring-library.md)
- **Tuning** — the resolution layer mapping a symbolic pitch (a scale step) to a frequency in Hz; 12-TET is the default, Scala-importable. · [signal-time-dsp](signal-time-dsp.md)
- **Value** — a latched, held, single-valued port form (`f32`/`enum`/`harmony`/`i32`), read as a constant within a `process` call via zero-order-hold. · [composition-operators](composition-operators.md)
- **Voice** — one instance of a voice instrument the Voicer runs; what sounds a note, distinct from the note Message itself. · [composition-operators](composition-operators.md)
- **Voice instrument** — an ordinary instrument whose interface makes it hostable by a Voicer; a role read off the interface, never a separate kind. · [composition-operators](composition-operators.md)
- **Voicer** — a runtime host (the first, and until ADR-0075 lands the only one): it builds N standalone voice patches and renders only the active ones per block. · [composition-operators](composition-operators.md)
- **Window** — the `reuben-api` crate: the one thing between the engine and every consumer, declaring the types a door serializes, the roster and its advertised sentences, and both ends of the structure channel. · [agent-mcp](agent-mcp.md)

## Avoid these synonyms

<!-- HAND-AUTHORED, not derived. The Glossary above holds one canonical term per topic; this list
     covers near-misses for more terms than the topic `## Terms` expose. `check_rules_derive.py`
     only rewrites `## Topics` and `## Glossary`. -->

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
- **Structure channel** — avoid: control channel, admin port, command socket. It carries control traffic as well as structure edits, but "control channel" still names the wrong thing: the distinction is loopback authoring door vs. OSC-the-wire foreign edge, not structure vs. control.
- **Gist-and-point** — avoid: duplicate-then-sync (the sweep is a backstop, not the mechanism), summary copy.
- **Render** — avoid: block time, process, audio callback (the callback is the host of Render, not Render itself).
- **Lane** — bare "lane" means **Delivery lane** (grounding). For polyphony say Voice, Channel, or Voice instrument — never "lane".
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
- **Signal** — avoid: audio buffer / control buffer (as distinct types), wire, carrier, read-view of a Float. (CV is a legitimate *use* of a Signal, not a competing type — see the glossary.)
- **Value** — avoid: param, scalar, control (as a distinct type), Float. Near-collision: the
  agent-facing word for what a value verb writes into an input or a pipe seed is lowercase
  "value", and it is *not* the `Value` Arg type — a symbol on an enum input is a value in that
  sense and no `Value` at all. Never write **`Value`-the-port-form**: say the input's value, or
  the pipe's value.
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
- **Intent word** — avoid: nudge. Retired: it is not a project term, and it misdescribes the ninth
  of the table that **assigns** rather than shoves (*sadder* sets a minor 3rd, *straighter* sets a
  rotation of 0, *more consonant* sets a snap policy). Also avoid: mood word, tweak word.
- **Output filter** — avoid: persona (ambiguous), style gate (deleted — the filter is taught, not enforced).
- **Push/pull delivery** — avoid: eager/lazy loading (runtime words for a prompt-architecture idea).

## Conventions

**Layout**

```
docs/rules/README.md                     index: topic summaries + derived glossary
docs/rules/<topic>.md                    now-story + rules; each rule links its rationale
docs/rules/rationale/<topic>/<rule>.md   condensed why + "Distilled from: …" / "Decided in: …"
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

**Parity marker** — a test whose job is that two lists match carries `Parity: <reason>` recording why
one cannot be generated from the other; `check_rules_refs.py` holds every marker to a substantive
reason. See [the rule](code-as-grounding.md#parity-test-is-a-defect-marker).

**Structural integrity** — `check_rules_links.py` walks the whole corpus in CI, not just the topic
docs: every link resolves (file *and* `#anchor`), rationale↔rule is a bijection, and every rationale
carries its provenance line. A rule anchor is therefore a **public identity** — rationale files link
each other and link back up to their rule, so renaming or deleting an anchor is a corpus-wide edit,
and the guard fails until it is one. This is what stops an absorption pass from leaving live prose
pointing at a rule that no longer exists.

**Claim ledger** — `extract_doc_claims.py` types every statement in the governed docs (this corpus,
`docs/agents/`, the root Markdown, the skills) that could be *wrong*. Paths, code identifiers and
`Guarded by:` lines are decided mechanically and gate CI on every commit; counts and single-sourcing
claims are ranked and routed to a reviewer (`--review`) and never gate, because a gate that guesses
at them is one people learn to ignore. Two asymmetries are deliberate: an identifier absent from
source is a finding in a rule and routine in a rationale (a rationale *argues*, and an argument names
what it **rejected**); and the entry docs (`AGENTS.md`, `README.md`) must name every path in full,
because that surface is read as navigation and an agent opens what it names.

**Derived index** — the Topics list and Glossary above are collated from the topic docs; do not
hand-edit them. The `scripts/hooks/pre-commit.d/30-rules-index` check regenerates them
(`check_rules_derive.py --write`) whenever a commit touches `docs/rules/`, and CI runs `--check` as
a backstop. Run `scripts/install-hooks.sh` once per clone — that is the one command that installs
the whole hook set, and [CONTRIBUTING.md](../../CONTRIBUTING.md) lists what is in it.

**Two owners, two markers, and the rules above the region are this repo's own.** A `derived —
collated from …` comment marks a section `check_rules_derive.py` collates from files in this repo. A
`ii:begin`/`ii:end` pair marks a span the doctrine generator renders from another repo, and it
carries a `sha256=` of itself so `scripts/ii_verify.py` can catch a hand-edit with no access to the
source. Neither is the `GENERATED from …` header, which belongs to a file that is generated whole.

**ADR lifecycle & the supersession marker** — see [docs/adr/README.md](../adr/README.md). An ADR
number is written down in exactly two places in this corpus: a rule's `Superseded by:` marker and a
rationale's `Distilled from:` line.

<!-- ii:begin company-doctrine — derived from templates/company-doctrine-rules.md + .ii/repo.toml; do not hand-edit out of sync. Regenerate with `ii-generate --write .`. sha256=3af7d0b3a83680c64643f174a05554ac8c323e112859fb5f4a1abe35f42c221c -->
## Company doctrine

The company's doctrine is stated here once, at the front door — not restated inside the documents that depend on it. What "doctrine" means is defined in the company glossary below, like every other company term.

### Company glossary

- **Company layer** — the Project the rest of the company draws on: facts, routing, cross-cutting work, and the machinery and assets other repos consume. Not one repo.
- **Doctrine** — what the company holds across its repos, as distinct from what any one repo decides for itself: the norms and definitions it states, the facts it requires every repo to state for itself, and the procedures it shares. It is authored in the company layer and reaches every repo that adopts it. **The kind decides where it is written.** A required fact is written into the repo, because a stranger — or a reader with none of this company's tooling installed — must still be able to navigate from it. A procedure is not written in: guidance that would read identically in every repo belongs with the tooling that carries it, and removing it strands no reader. **A norm is written in** — it states a rule rather than a how-to, and may not be dropped as though it were a procedure.
- **Obligation** — an external commitment with a deadline whose state is held in no other system. Tracked as an issue in the company layer.
- **PRD** — **retired.** It arrived as template residue and imports a product-management frame (requirements handed down by a product manager) that does not describe how work starts. The tooling had already settled it in practice. The word is still in most repos' docs; where you meet it, it means **spec**. Decided 2026-08-06.
- **Piece / Element** — reserved. The Community Garden uses them for its individual installations and is canonical for what they mean. **Do not use these words at company altitude** to mean anything else. The company layer names Projects and nothing inside them — what a Project calls its internals is that Project's vocabulary.
- **Project** — the company-altitude unit: a line of work that could succeed or fail independently of the rest of the company. It may have zero, one, or several repos, and that set changes over time — **repos are assets a Project accrues, never its identity.** An object with no code is a Project. So is the website.
- **Reach** — whether a source can be read by a headless agent, an interactive session, or only by a human.
- **Source of truth / canonical** — the one place a fact actually lives. Every other appearance of that fact is a copy and must say so.
- **Spec** — a piece of work described completely enough that an agent can implement it without an interview: the problem, the solution, the decisions already made, and what is out of scope. Usually written up from a conversation already had. **Every spec lives in an issue; not every issue holds a spec** — the issue is the container, the record of anything with an open→closed arc, and the spec is what its body says. A one-line bug report holds no spec, and an obligation rarely does. Say `spec` for the document, `issue` for what holds it; the pair is company-wide, not a per-repo setting.
- **Venture / Product** — **retired.** Both were proposed on 2026-08-05 and neither decided anything; "Project" replaces them everywhere. If you find either word still in use in this org, it means Project.

The company glossary is authored elsewhere and read at generation time. Its provenance:

```
# Source:   Impractical-Instruments/brain@440034f0365465428b89668736b6ae506e7c564c:CONTEXT.md
# Fetched:  2026-08-15
# Refresh:  gh api repos/Impractical-Instruments/brain/contents/CONTEXT.md --jq '.content' | base64 -d
# Do not edit locally. Changes go upstream via PR against the source repo.
```

**A company term means what the glossary says it means.** Redefining one for local use fails the build — two live definitions of the same word is the drift this whole arrangement exists to delete. **Extending is free:** a term this repo needs and the glossary does not carry is yours to define locally, under a name the glossary has not already claimed.

### Use the glossary's vocabulary

When your output names a domain concept — an issue title, a refactor proposal, a hypothesis, a test name — use the term as the glossary defines it. Do not drift to a synonym, and do not reach for a term the glossary has retired.

If the concept you need is not in the glossary, that is a signal, not a licence to invent a word. Either you are naming something the project does not actually have — reconsider — or there is a real gap, and the fix is to add the term to the glossary rather than to work around it.

### Work from a fresh worktree, not from the clone you are standing in

A main clone is a person's workspace. It sits wherever its owner left it — behind its remote, on a feature branch, mid-review — and **that is its ordinary state, not a fault to correct.** An agent that works there silently inherits whatever it finds, then reports the past with total confidence and no error.

So agent work starts by cutting a **worktree from a freshly fetched default branch**, and lands there. Another base is right where the work genuinely belongs to it — a stacked change, a release branch, a fix on top of something unmerged — but that is a deliberate choice, and the default is the default branch.

Two things that follow, and both get done by accident otherwise. **Do not `git pull` someone's main clone to make room for your work** — its state is theirs, not yours to reconcile. And **do not branch off whatever it is currently sitting on**; that is the same mistake with an extra step, and it inherits the staleness while looking like a fresh start.

Reading a file out of a stale checkout and reporting what it says is the failure this prevents. A working copy cannot tell you it is out of date.

### A broken environment is a finding, not an obstacle

When the environment does not behave — a variable the docs say is set and is not, a missing tool, a hook that never fires, a script that announces it is a skeleton — **the first hypothesis is that this machine is behind the blessed setup, and the first move is to check.** Not to route around it.

Routing around it costs three things at once. The symptom is hidden rather than fixed. The next agent rediscovers it from scratch, and pays again. And where the missing piece was a **guard**, working around it is indistinguishable from switching it off — the work continues, unguarded, and nothing says so.

Repairing your own environment by hand, for the length of one command, is the failure and not the fix. Repair it where the setup lives, so the next session inherits the repair.

### Every open issue says who acts next

An open issue carries exactly one state role: `ready-for-agent` or `ready-for-human`. The issue-tracker document holds the vocabulary; the obligation is here.

**Whoever files applies it, and an agent applies it at filing rather than deferring** — an agent knows whether it could have done the work, and that judgement is expensive to reconstruct from a title later. Filing without a role is a human's prerogative, not an agent's.

**`ready-for-agent` asserts the body is implementable without an interview, and the dispatcher acts on it.** Applying it starts unattended work, so it is applied by someone who read the body and **never mechanically**: no script, hook, template or scheduled job adds a state role. A checker may report that a role is missing — choosing one is not mechanisable.

`ready-for-human` means a human is in the loop, including work an agent could mostly do. It is not a holding pen for unread issues; an issue carrying no role is already that, and the dispatcher cannot see it.

### Referring to another repo's decisions

A cross-repo reference to a decision **names the topic and resolves against a pinned ref** — never a number.

Numbers are repo-local: `0001` means a different decision in every repo that has one, so a bare ADR number read from another repo points at whatever happens to sit at that index. Within a single repo a number is not reliably an identifier either — nothing enforces that one is used once. And where a repo distils settled ADRs into rules and deletes them, the numbered document a reference names may not survive at all. A topic name resolved at a pinned commit survives all of that.

### Canonical sources and provenance

How facts move between repos and systems without drifting.

1. **Read on demand.** Fetch the canonical source at the moment you need it. Store nothing locally. A live read is cheaper than a pipeline and cannot go stale.
2. **Pointers, never values.** A routing document routes; it holds zero copied facts. Test before adding any line anywhere: *"if someone edited only this line and not the source, would anything be wrong?"* If yes, it is a value — do not write it there.
3. **One direction.** A consumer reads from its sources. It never writes to them.
4. **Every unavoidable copy declares its provenance.**

Some consumers genuinely cannot take a live dependency — firmware, a Godot or TouchDesigner installation, an offline build. Vendoring is allowed. Silent vendoring is not. Take the highest rung you can reach:

| Rung | What it means | Drift risk |
|---|---|---|
| **Dependency** | Package, submodule, or import resolved at build time | None |
| **Vendored with provenance** | A copy that records where it came from and how to refresh | Visible |
| **Freeform copy** | Someone pasted it | Silent — **not allowed** |

A vendored artifact carries a provenance block beside it, naming its source, the commit, the date it was fetched, and the exact command that refreshes it. That command has to run from a bare clone of this repo: one that assumes a sibling checkout of another repo is a declared copy whose declaration does not work, which is the freeform rung wearing the middle rung's label.

Provenance is what converts silent drift into visible staleness. It is the only thing that makes a copy safe.

**Referring to another repo's decisions** and **Canonical sources and provenance** are restated here, not authored here. The wording is deliberately generalised — it names no repo-local path, so it stands in a repo that is not the one it came from — which is why it is not word-for-word with its source. Both sides of the restatement are pinned. The generator refuses to regenerate if this wording changes without being reconciled again, which it checks without reaching the source at all; and where it can reach the source, it reads the revision named below and checks its bytes against a digest of them. A run that could not reach the source says so in the block rather than naming a revision it did not read.

```
# Source:   Impractical-Instruments/brain@62c7b8e402e6fccefff8a2039d3328af1d3217fe:docs/agents/canonical-sources.md
# Fetched:  2026-08-12
# Refresh:  gh api 'repos/Impractical-Instruments/brain/contents/docs/agents/canonical-sources.md?ref=62c7b8e402e6fccefff8a2039d3328af1d3217fe' --jq '.content' | base64 -d
# Restated, not copied. The wording is authored in the doctrine template; this names the revision it was reconciled with.
```
<!-- ii:end company-doctrine -->
