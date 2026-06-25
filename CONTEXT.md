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
_Avoid_: engine, manager, host (a host embeds the system; the Coordinator owns the graph).

**Plan**:
The static parallel execution schedule — topologically ordered and clustered for parallelism — that the engine runs. Produced by instantiating a graph description; consumed by [[render]]; replaced by [[swap]].
_Avoid_: schedule, graph image, compiled graph.

**Swap**:
The single runtime transition that changes the running graph: instantiate a new [[plan]] off the audio thread, atomically install it at a block boundary, migrate surviving Operators' state (matched by stable identity), and reclaim the old Plan. The first Swap installs over the empty Plan, so there is no separate cold-start path. "Instantiate" is the construction sub-step of a Swap.
_Avoid_: hot-swap (describes how, not the phase), re-plan, recompile, reload.

**Render**:
Executing the current [[plan]] per block on the audio thread — hard realtime, allocation-free. Playing notes and turning knobs happen here against already-allocated resources.
_Avoid_: block time, process, audio callback (the callback is the host of Render, not Render itself).

**Lane**:
One concrete signal path through the Voice×Channel fan-out — a single [[voice]] in a single [[channel]]. A 16-voice stereo point in the graph has 32 Lanes. Operators are authored single-Lane (one mono stream a block at a time); the engine replicates them across all Lanes with per-Lane state. Lane count can expand or collapse along the graph.
_Avoid_: stream, tap, scalar.

**Voice**:
One independent sounding instance within a polyphonic [[instrument]] — its own envelope, filter, oscillator phase, etc. Voices come from a pre-allocated pool bounded at instantiation; a [[voicer]] assigns notes to Voices and applies the stealing policy. A Voice is *not* a [[channel]] — a single Voice may span several Channels (e.g. a stereo Voice).
_Avoid_: channel, note (a note is a Message; a Voice is what sounds it).

**Channel**:
One discrete signal path in n-channel I/O — a speaker output, an input, or one lane of a multichannel signal. Orthogonal to [[voice]].
_Avoid_: voice, bus.

**Voicer**:
The Operator that assigns incoming note Messages to [[voice]]s from a pre-allocated pool and applies the voice-stealing policy (default: steal oldest with a quick release to avoid clicks). Where polyphony is managed.
_Avoid_: allocator, poly, note manager.

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
Both a principle and an artifact. *As principle*: every control is hard to make sound bad — energy in produces juicy musical feedback out, easy defaults always provided. *As artifact*: a curated, often mapped control on an [[instrument]]'s surface (e.g. one "brightness" knob fanned to filter cutoff + resonance, each over its own range) — built from composition (`map` Operators + Message fan-out), not a special type. A Good Button (artifact) embodies the Good Button (principle).
_Avoid_: meta param, meta-control, macro (all name the artifact — say Good Button).

**Signal**:
Shorthand for a [[message]] whose [[arg]] is a `Buffer` — one block of contiguous samples per [[channel]], flowing between Operators; the dense port type an Operator declares when it processes per-sample. Not a type of its own: `Signal<f32>` is the only element kind today, but the model is architected so other `Signal<T>` kinds can exist. Audio, CV, and control are all a `Buffer`.
_Avoid_: CV, audio buffer / control buffer (as distinct types), wire, carrier, read-view of a Float.

**Buffer**:
The contiguous-memory payload of a [[signal]] — the most performant representation of a per-sample stream, and one of the [[arg]] kinds. It does not implement OSC conversion, so audio cannot cross the OSC boundary by construction.
_Avoid_: arena, sample array, f32 slice (as the domain term).

**Message**:
A discrete, OSC-shaped payload: an `address` path + a sample `frame` timestamp + exactly one [[arg]]. Carries notes, chords, triggers, gestures, parameter values, dense audio (as a `Buffer` Arg), and all external I/O. The lingua franca, internal and external — an internal Message and an external OSC packet are the same idea, reconciled by explicit boundary conversion (external OSC carries multiple args and no timestamp). The `address` serves OSC shape, boundary routing, and debug — never internal dispatch.
_Avoid_: event, control, OSC packet (as a distinct internal type), typed args (plural — a Message holds exactly one Arg).

**Input**:
One functional value an Operator consumes, declared once with an [[arg]] type and an unwired default. Fed by a literal or by a wire from another Operator's output — the same slot takes either. Replaces the old split of Signal port / param / connection / context port: one Input per function.
_Avoid_: port, param, connection, slot (the slot is the Input; its payload is the [[arg]]).

**Arg**:
The single typed payload of a [[message]], and the type an [[input]]/output declares — what replaced the old "shape" axis (delivery and read-style now follow from the Arg type plus the read verb, never declared separately). One closed, central enum: an OSC primitive (`F32`/`I32`/`Str`), a shared [[vocab]] concrete type (`Note`/`Harmony`/`FilterMode`/`Waveform`/…), or a `Buffer`. Concrete types exist *because* a Message holds exactly one Arg — two scalars (pitch+velocity) pack into one `Arg::Note`. Enums read as real Rust enums in operator code (`FilterMode::HighPass`), not bare indices. Crossing from one Arg type to another is an explicit converter Operator, never implicit coercion — the one exception being `F32`→`Buffer`, which ZOH-materializes automatically.
_Avoid_: shape, kind, PortKind, value, blob, carrier, port.

**vocab**:
The shared module of concrete [[arg]] types — `Note`, `Harmony`, `Pitch`, `FilterMode`, `Waveform`, `M2sMode`, `MapCurve`, … — each defined once and reused everywhere (a `FilterMode` duplicated per-operator would be the code smell, and would force `Arg` open). Each `#[derive(ArgValue)]` generates its OSC `to/from` conversion, `Arg` integration, and metadata. New domain type = declare it in `vocab`, derive, add one line to `Arg`.
_Avoid_: enum registry, type table, concrete-arg module.

**Held value (ZOH latch)**:
A port's current value: the last [[message]]'s [[arg]] on that port, held until it changes (zero-order-hold). One per-port latch — the single mechanism behind every "current" read, collapsing the former separate Harmony, enum, and param lanes into one. Stored `Copy`-normalized (a held enum holds its resolved value, never a `String`) to stay allocation-free on the audio thread.
_Avoid_: context, param lane, enum latch (as separate mechanisms), state.

**Constant**:
Instantiate-time configuration of an Operator instance that never changes on the data path. The line is exact: a value is a Constant iff changing it would rebuild the graph — e.g. `voices`, which sets [[lane]] count and topology. Lives in an Operator's `config` block, not its [[input]]s. [[arg]] type alone does not make a Constant: a live-switchable enum like filter mode is an [[input]], not a Constant.
_Avoid_: param, setting, option, config value.
