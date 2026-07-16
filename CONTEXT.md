# reuben

A configurable musical instrument built from composable **Operators** that each do something simple, and produce complex musical behavior when combined. Easy for beginners via pre-built **Toys**; deeply customizable underneath. Music data is the primary payload, but the same data can drive anything controllable over time (lights, video, game engines). OSC is the lingua franca, internally and externally.

## Language

**Operator**:
The smallest composable unit of behavior. Does one simple thing; combined with others, yields complex musical results.
_Avoid_: node, object, module, block (block = an audio buffer chunk), ugen, plugin.

**Instrument**:
A named subgraph of Operators patched into one playable thing, exposing boundary ports (gestures/Messages in; music data and/or audio out). Because it exposes ports, an Instrument can be reused inside another Instrument or a [[rig]] as if it were an Operator (composition is recursive). The unit you actually play. Generators and effects are all Instruments — in reuben you "play" an effect too.
_Avoid_: patch (noun — see Patch), device, rack, module.

**Rig**:
A full playable system: multiple Instruments wired together with routing (n-channel, OSC). The thing a performer runs end to end.
_Avoid_: project, set, session, scene, song.

**Patch** (verb):
To wire Operators or Instruments together. "Patch" is the act; the result is an Instrument or a Rig — never "a patch."
_Avoid_: patch as a noun.

**Toy**:
A ready-made [[instrument]] (or [[rig]]) designed for instant play — e.g. groove box, tap-to-play chord/melody player, drag/strum instrument, meta-effect. The on-ramp for beginners and non-musicians.
_Avoid_: preset, template.

**Address**:
The OSC path that names an Operator, [[input]], or output. Auto-derived from graph structure by default; an [[instrument]] may also expose stable curated addresses as its public control surface. Wildcards (`/drums/*/decay`) match many targets at once.
_Avoid_: path, route, id (id = internal identity, not the address).

**Coordinator**:
The single non-realtime owner of the canonical graph and Instrument library — the only writer of graph structure. Receives edit commands, performs Instantiate and [[swap]], and reclaims retired [[plan]]s off-thread. [[render]] only ever reads the current immutable Plan.
_Avoid_: engine, manager, host in the system-embedder sense (a host application embeds the system; the Coordinator owns the graph — the [[voicer]]'s [[host]] path is a different, sanctioned sense).

**Plan**:
The static parallel execution schedule — topologically ordered and clustered for parallelism — that the engine runs. Produced by instantiating a graph description; consumed by [[render]]; replaced by [[swap]].
_Avoid_: schedule, graph image, compiled graph.

**Swap**:
The single runtime transition that changes the running graph: instantiate a new [[plan]] off the audio thread, atomically install it at a block boundary, migrate [[survivor]]s' state, and reclaim the old Plan. The first Swap installs over the empty Plan, so there is no separate cold-start path. "Instantiate" is the construction sub-step of a Swap.
_Avoid_: hot-swap (describes how, not the phase), re-plan, recompile, reload.

**Survivor**:
A node whose state crosses a [[swap]]: present in both the outgoing and incoming [[plan]] with the same fully-qualified [[address]], the same Operator type, and the same instantiate-time identity (its config and the content of everything it resolved — resources, hosted sub-documents). Anything else about it — wiring, params, neighbors — may change without breaking survivorship.
_Avoid_: carried node, kept node, matched node.

**Restart-swap**:
The M1 interim [[swap]]: stop the audio streams, rebuild the engine from the document, reopen. Audibly rude by design — a gap, every node cold — and replaced in M2 by the real Swap machinery behind the same contract.
_Avoid_: reload, hot restart.

**Structure channel**:
The local request/response channel between a client (the MCP sidecar) and the engine process — the only path to the [[coordinator]]. Carries structure ops ([[swap]]), document reads, diagnostics, and liveness; distinct from the fire-and-forget OSC control plane, which never carries structure.
_Avoid_: control channel (that is OSC), admin port, command socket.

**Gist-and-point**:
The posture for prose at a secondary surface (the MCP server's `instructions` and tool descriptions, a skill's workflow steps): never restate the canonical contract — carry the one-breath gist and point at the one doc that holds the rules (the authoring guide, served as `reuben://guide/authoring`). Duplication is a drift pair; a pointer can't drift.
_Avoid_: duplicate-then-sync (the sweep is a backstop, not the mechanism), summary copy.

**Render**:
Executing the current [[plan]] per block on the audio thread — hard realtime, allocation-free. Playing notes and turning knobs happen here against already-allocated resources.
_Avoid_: block time, process, audio callback (the callback is the host of Render, not Render itself).

**Lane** _(retired)_:
Formerly one concrete path through a Voice×Channel fan-out the engine replicated operators across. Removed: polyphony now comes from a [[voicer]] hosting [[voice instrument]]s, and [[channel]]s fan out only at the master output. The term survives only in frozen ADRs (e.g. 0010, 0032) — don't use it for new work; say [[voice]], [[channel]], or [[voice instrument]].

**Voice**:
One independent sounding instance within a polyphonic [[instrument]] — its own envelope, filter, oscillator phase, etc. Voices come from a pre-allocated pool bounded at instantiation; a [[voicer]] assigns notes to Voices and applies the stealing policy. A Voice is *not* a [[channel]] — a single Voice may span several Channels (e.g. a stereo Voice).
_Avoid_: channel, note (a note is a Message; a Voice is what sounds it).

**Channel**:
One discrete signal path in n-channel I/O — a speaker output, an input, or one channel of a multichannel signal. Orthogonal to [[voice]].
_Avoid_: voice, bus.

**Voicer**:
The Operator that assigns incoming note Messages to [[voice]]s from a pre-allocated pool and applies the voice-stealing policy (default: steal oldest with a quick release to avoid clicks). Hosts the [[voice instrument]]s and sums their audio. Where polyphony is managed.
_Avoid_: allocator, poly, note manager.

**Voice instrument**:
A standalone [[instrument]] whose [[interface]] carries the voice face — `freq`/`gate` in, `audio`/`active` out — which is all that makes it hostable by a [[voicer]] as one [[voice]] of its pool ([[interface makes the role]]). Referenced by path, instantiated once per Voice; carries a Voice's per-instance signal chain (oscillator, filter, envelope). The same document, statically nested via a `subpatch` node, is just a nested instrument — the role is contextual.
_Avoid_: voice sub-patch (retired — role, not kind), voice graph, sub-instrument, voice template.

**Interface**:
An [[instrument]]'s engine-honored I/O boundary: named, typed **pipes** — an input pipe mints an address internal Operators consume; an output pipe is fed from an internal port — type-checked and wired by the engine. Real wiring carrying the quantity contract (type/default/range/curve/unit), never surface metadata: the one boundary a [[surface doc]] binds by name. A [[voice instrument]]'s `freq`/`gate`/`audio`/`active` boundary is the canonical case.
_Avoid_: control surface, ports block.

**Interface makes the role**:
The naming principle for reusable documents (ADR-0057): there is one noun — [[instrument]] — and roles are contextual, read off the [[interface]] or the use, never off a path, filename, or kind. Hosted by a [[voicer]] → a [[voice instrument]], while hosted; referenced via a `subpatch` node → a nested instrument, while referenced. An instrument's **recipe-role** — its reuse story — is its `doc` first line plus its interface face, projected into the generated library index: trusted for selection, never for wiring (the face is always generated from the `interface` block).
_Avoid_: recipe as a kind of document, role-by-directory, naming conventions for role.

**Subpatch**:
A format keyword, not a domain noun (ADR-0057): the built-in node type whose `patch` slot names an [[instrument]] in `resources`, referencing it as a nested instrument inside another. Statically nested — exactly one instance, fixed at build — so it [[inline (dissolve)]]s; a [[voice instrument]] is the dynamic counterpart, [[host]]ed by a [[voicer]]. The referenced document is an ordinary instrument; say "nested instrument" for the thing, `subpatch` only for the node.
_Avoid_: subpatch as a noun for the document, sub-instrument, nested patch, embedded instrument.

**Inline (dissolve)**:
The build-time splice that flattens a [[subpatch]] into its parent: child nodes spliced in, addresses namespace-prefixed, boundary wires rewired to the inner targets, the subpatch node gone before [[render]]. The fixed-cardinality half of the line: fixed-at-build → inline; runtime-varying → [[host]].
_Avoid_: expand, flatten (as the term of art), instantiate.

**Host**:
The runtime nesting path: an Operator keeps each nested instance as its own [[plan]] and renders it re-entrantly per block. The [[voicer]] is the sole host — [[voice]]s come and go, which a build-time splice can't express. The dynamic counterpart of [[inline (dissolve)]]; distinct from the avoided system-embedder sense of "host" (see [[coordinator]]).
_Avoid_: runtime nest, sub-plan path (informal).

**Boundary face**:
The synthesized port set a [[subpatch]] presents, computed at load from its child's [[interface]]: one port per interface name, each carrying the pipe's declared [[arg]] type and quantity contract (an output pipe inherits type from the internal port that feeds it); presentation lives in a [[surface doc]], never on the face. A build-time and introspection artifact only — it dissolves with the node and never reaches [[render]].
_Avoid_: descriptor (the compile-time operator contract), synthesized ports (informal).

**Surface doc**:
A presentation-only JSON document (`surfaces/<name>.json`) binding an [[instrument]]'s [[interface]] input pipes to widgets by name — label, widget kind, grouping, order, optionally a narrower range. It carries no contract: the pipe owns the quantity, a resolver merges it at load (so surfaces never drift from the boundary), and with no doc a default surface derives from the pipes. Durable and editable, and portable across hosts: the `.tosc` layout is a disposable projection of it, while a host with its own renderer (the browser player) reads it directly.
_Avoid_: control block (the retired inline per-node form), layout file, UI config, `.tosc` (a projection of the doc, not the doc).

**Superset widget vocabulary**:
The one shared set of widget names a [[surface doc]] may use — deliberately a superset of what any single target renders (shipped: fader, radial, param-toggle, note-toggle, chord-button; reserved: xy-pad, grid, visualizer, keyboard). Each target renders its subset and skips the rest loudly, so no target's ceiling caps another's.
_Avoid_: widget list, control types, per-target vocabulary (the vocabulary is shared; only rendering is per-target).

**Surface pipe promotion**:
Rewriting a control that lived inline in the graph (a retired `control` block, a `map` instance literal) as a named [[interface]] input pipe carrying the quantity contract — giving a [[surface doc]] an honest, engine-validated name to bind. A graph edit, not a presentation edit; a sequencer's N steps promote to N ordinary pipes, no new machinery.
_Avoid_: exposing a param (informal — say promotion), control migration, lane pipe (shelved future sugar, not this).

**Pitch**:
A symbolic value, modeled as `enum { Degree(i32), Absolute(f32) }` (no invalid states) — primarily a scale `Degree` within the active [[scale]], with `Absolute` float MIDI note (60.0 = middle C) available as a 12-TET coordinate. Symbolic only; a [[tuning]] resolves it to a frequency in Hz. A **Note** is `{ pitch: Pitch, velocity: f32 }` (velocity 0 = note-off).
_Avoid_: note number (alone), frequency (frequency is the resolved result, not the Pitch).

**Tuning**:
The layer that resolves symbolic [[pitch]] to frequency in Hz. Imported from Scala `.scl`/`.kbm`; supports any non-Western or user-defined system. 12-TET is just the default Tuning. Rides the [[harmony]] bus, so it can change in real time while notes sound.
_Avoid_: temperament, scale (scale = which degrees are in play; Tuning = their frequencies).

**Scale**:
The set of degrees currently in play and the active key/mode — the "which notes exist" layer that melodic Operators snap to. Distinct from [[tuning]] (which gives those degrees their Hz).
_Avoid_: mode (a mode is one kind of Scale), key (key is part of the Scale).

**Harmony**:
The current tonal frame — current key/[[scale]], current chord, and active [[tuning]] — broadcast on a bus that Operators subscribe to and snap to, and read on the operator side as a held, `Copy` struct (root/scale/chord + resolvers) carried as the `Harmony` [[arg]]. The good-button harmony engine — change the broadcast and every follower re-harmonizes.
_Avoid_: tonal context, context, harmony bus, key signature.

**Clock**:
The source of base musical timing — tempo, meter, position. A global default Clock keeps everything in sync out of the box; Clocks are also Operators, so independent or polytempo timing can be patched. Provides timing only — groove, swing, and feel are separate Operators.
_Avoid_: transport, master clock, conductor.

**Good Button**:
Both a principle and an artifact. *As principle*: every control is hard to make sound bad — energy in produces juicy musical feedback out, easy defaults always provided. *As artifact*: a curated, often mapped control on an [[instrument]]'s surface (e.g. one "brightness" knob fanned to filter cutoff + resonance, each over its own range) — built from composition (`map` Operators + Message fan-out), not a special type, exposed to the player as an [[interface]] input pipe and presented by a [[surface doc]]. A Good Button (artifact) embodies the Good Button (principle).
_Avoid_: meta param, meta-control, macro (all name the artifact — say Good Button).

**Signal**:
One of the three port **forms** (with [[value]] and [[event]]): a dense per-sample [[buffer]] — one block of contiguous samples per [[channel]] — flowing between Operators at audio rate, declared `f32_buffer`. Audio, CV, and swept controls are all a Signal. Read per-sample; a held [[value]] wired into a Signal input materializes one.
_Avoid_: CV, audio buffer / control buffer (as distinct types), wire, carrier, read-view of a Float.

**Value**:
One of the three port **forms** (with [[signal]] and [[event]]): a single held scalar, enum, or [[harmony]], declared `f32` (or a [[vocab]] type), latched ([[held value (zoh latch)]]) and read once per block-slice. The form of a knob, a gate, a tonal frame. A Value may feed a [[signal]] input (it materializes); the reverse — [[signal]] into a Value — is a hard error, since there is no implicit sample-and-hold.
_Avoid_: param, scalar, control (as a distinct type), Float.

**Event**:
One of the three port **forms** (with [[signal]] and [[value]]): a sparse, frame-stamped, unlatched stream of [[message]]s, declared as a [[vocab]] event type (`Note`). Many may land at one frame — a chord's tones all survive, none collapsed. The form a sequencer emits and a [[voicer]] reads.
_Avoid_: trigger, stream (as a type), notes (plural, as a type).

**Buffer**:
The contiguous-memory payload of a [[signal]] — the most performant representation of a per-sample stream, and one of the [[arg]] kinds (spelled `f32_buffer`). It does not implement OSC conversion, so audio cannot cross the OSC boundary by construction.
_Avoid_: arena, sample array, f32 slice (as the domain term).

**Message**:
A discrete, OSC-shaped payload: an `address` path + a sample `frame` timestamp + exactly one [[arg]]. Carries notes, chords, triggers, gestures, parameter values, dense audio (as an `f32_buffer` Arg), and all external I/O. The lingua franca, internal and external — an internal Message and an external OSC packet are the same idea, reconciled by explicit boundary conversion (external OSC carries multiple args and no timestamp). The `address` serves OSC shape, boundary routing, and debug — never internal dispatch.
_Avoid_: event, control, OSC packet (as a distinct internal type), typed args (plural — a Message holds exactly one Arg).

**Input**:
One functional value an Operator consumes, declared once with an [[arg]] type and an unwired default. Fed by a literal or by a wire from another Operator's output — the same slot takes either. Replaces the old split of Signal port / param / connection / context port: one Input per function.
_Avoid_: port, param, connection, slot (the slot is the Input; its payload is the [[arg]]).

**Handle**:
The typed `In`/`Out` const the operator contract emits per [[input]]/output (`IN_FREQ`, `OUT_AUDIO`). Its *type* names the port's form ([[signal]] / held [[value]] / [[event]] / raw pass-through), so `io.read`/`io.write` return the right shape by construction and a wrong-form access does not compile; its value carries the declared default, which is the held read's fallback. Named `In`/`Out` — never `InPort`/`OutPort` ("port" stays avoided; the domain word is [[input]]).
_Avoid_: port handle, index const (the handle replaced the bare `usize` const), port.

**Arg**:
The single typed payload of a [[message]], and the type an [[input]]/output declares — what replaced the old "shape" axis (delivery and read-style now follow from the Arg type plus the read verb, never declared separately). One closed, central enum: an OSC primitive (`F32`/`I32`/`Str`), a shared [[vocab]] concrete type (`Note`/`Harmony`/`FilterMode`/`Waveform`/…), or an `f32_buffer`. Concrete types exist *because* a Message holds exactly one Arg — two scalars (pitch+velocity) pack into one `Arg::Note`. Enums read as real Rust enums in operator code (`FilterMode::HighPass`), not bare indices. Crossing from one Arg type to another is an explicit converter Operator; the one implicit coercion is a held [[value]] materializing into a [[signal]] buffer, and its reverse ([[signal]]→[[value]]) is a hard error.
_Avoid_: shape, kind, PortKind, value, blob, carrier, port.

**vocab**:
The shared module of concrete [[arg]] types — `Note`, `Harmony`, `Pitch`, `FilterMode`, `Waveform`, `M2sMode`, `MapCurve`, … — each defined once and reused everywhere (a `FilterMode` duplicated per-operator would be the code smell, and would force `Arg` open). Each `#[derive(ArgValue)]` generates its OSC `to/from` conversion, `Arg` integration, and metadata. New domain type = declare it in `vocab`, derive, add one line to `Arg`.
_Avoid_: enum registry, type table, concrete-arg module.

**Held value (ZOH latch)**:
A port's current value: the last [[message]]'s [[arg]] on that port, held until it changes (zero-order-hold). One per-port latch — the single mechanism behind every "current" read of a [[value]] port, collapsing the former separate Harmony, enum, and param latches into one. Stored `Copy`-normalized (a held enum holds its resolved value, never a `String`) to stay allocation-free on the audio thread.
_Avoid_: context, param latch, enum latch (as separate mechanisms), state.

**Constant**:
Instantiate-time configuration of an Operator instance that never changes on the data path. The line is exact: a value is a Constant iff changing it would rebuild the graph — e.g. `voices`, which sets a [[voicer]]'s [[voice]]-pool size (how many [[voice instrument]]s it hosts). Declared with the contract's `constant:` keyword; lives in an Operator's `config` block, not its [[input]]s. [[arg]] type alone does not make a Constant: a live-switchable enum like filter mode is an [[input]], not a Constant.
_Avoid_: param, setting, option, config value.

**Delivery lane**:
One of the three consumer paths for authoring grounding — repo skills (checkout, pointers), MCP clients (in-band resources), web chat (bundled at build). A lane is a transport, never a content author: sauce is authored once and delivered per lane (ADR-0059). Distinct from the retired DSP sense of [[lane]].
_Avoid_: surface (that is a presentation doc), channel (that is signal I/O), bare "lane" without context.

**Input handling**:
The lane-shared half of the chat sauce: interpreting musical/mood/abstract language as patching moves — the word→move table (ADR-0058) plus the edge conduct around imperfect mappings (ambiguous → act on the most-likely reading and offer alternatives; unsatisfiable → nearest achievable move). Identical in every [[delivery lane]]; a dev says "warmer" too.
_Avoid_: intent parsing, NLU.

**Output filter**:
The host-owned half of the chat sauce: what the person is shown — the sound-not-machine subject rule, hidden failures, silent tool planning, the register ratchet, naming, tone. Zero at skills/MCP, maximal at web; "persona" means this filter. It is a composable host module, never lane sauce (ADR-0059).
_Avoid_: persona (ambiguous), style gate (deleted, ADR-0005 — the filter is taught, not enforced).

**Push/pull delivery**:
The cost shape of a [[delivery lane]]: **push** = bundled into context, paid every session (web's only channel); **pull** = pointer or resource, free until followed (skills, MCP). The delivery rule: push only what earns its keep every session; pull everything else; for a push-only lane, omission is a cost decision, not a redundancy claim (ADR-0059).
_Avoid_: eager/lazy loading (runtime words for a prompt-architecture idea).
