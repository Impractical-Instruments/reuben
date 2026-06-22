# reuben

A configurable musical instrument built from composable **Operators** that each do something simple, and produce complex musical behavior when combined. Easy for beginners via pre-built **Toys**; deeply customizable underneath. Music data is the primary payload, but the same data can drive anything controllable over time (lights, video, game engines). OSC is the lingua franca, internally and externally.

## Language

**Operator**:
The smallest composable unit of behavior. Does one simple thing; combined with others, yields complex musical results.
_Avoid_: node, object, module, block (block = an audio buffer chunk), ugen, plugin.

**Instrument**:
A named subgraph of Operators patched into one playable thing, exposing boundary ports (gestures/Messages in; music data and/or audio out). Because it exposes ports, an Instrument can be reused inside another Instrument or a [[rig]] as if it were an Operator (composition is recursive). The unit you actually play. Generators, effects, and meta-effects are all Instruments — in reuben you "play" an effect too.
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
The OSC path that names an Operator, port, or param. Auto-derived from graph structure by default; an [[instrument]] may also expose stable curated addresses as its public control surface. Wildcards (`/drums/*/decay`) match many targets at once.
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
A symbolic value — primarily a scale degree within the active [[scale]], with float MIDI note (60.0 = middle C) available as a 12-TET coordinate. Symbolic only; a [[tuning]] resolves it to a frequency in Hz.
_Avoid_: note number (alone), frequency (frequency is the resolved result, not the Pitch).

**Tuning**:
The layer that resolves symbolic [[pitch]] to frequency in Hz. Imported from Scala `.scl`/`.kbm`; supports any non-Western or user-defined system. 12-TET is just the default Tuning. Rides the [[tonal-context]] bus, so it can change in real time while notes sound.
_Avoid_: temperament, scale (scale = which degrees are in play; Tuning = their frequencies).

**Scale**:
The set of degrees currently in play and the active key/mode — the "which notes exist" layer that melodic Operators snap to. Distinct from [[tuning]] (which gives those degrees their Hz).
_Avoid_: mode (a mode is one kind of Scale), key (key is part of the Scale).

**Tonal context**:
The broadcast Messages carrying the current key/[[scale]], current chord, and active [[tuning]], which Operators subscribe to and snap to. The good-button harmony engine — change the broadcast and every follower re-harmonizes.
_Avoid_: harmony bus, key signature.

**Clock**:
The source of base musical timing — tempo, meter, position. A global default Clock keeps everything in sync out of the box; Clocks are also Operators, so independent or polytempo timing can be patched. Provides timing only — groove, swing, and feel are separate Operators.
_Avoid_: transport, master clock, conductor.

**Meta-effect**:
A control that targets many destinations at once via wildcard [[address]] matching — one gesture, many params. The mechanism behind "effect racks."
_Avoid_: macro, rack (rack is a kind of meta-effect, not a separate mechanism).

**Good Button**:
Both a principle and an artifact. *As principle*: every control is hard to make sound bad — energy in produces juicy musical feedback out, easy defaults always provided. *As artifact*: a curated, often mapped control on an [[instrument]]'s surface (e.g. one "brightness" knob fanned to filter cutoff + resonance, each over its own range) — built from composition (`map` Operators + Message fan-out), not a special type. A Good Button (artifact) embodies the Good Button (principle).
_Avoid_: meta param, meta-control, macro (all name the artifact — say Good Button).

**Signal**:
A continuous audio-rate float buffer flowing between Operators — one block of samples per channel. CV and audio are the same thing: both are Signals. Sub-audio-rate control is not a Signal; it is a Message.
_Avoid_: CV, audio buffer / control buffer (as distinct types), wire.

**Message**:
A discrete, OSC-shaped payload: an address path + typed args + a sample-accurate timetag. Carries notes, chords, triggers, gestures, parameter values, and all external I/O. The lingua franca, internal and external — an internal Message and an external OSC packet are the same shape.
_Avoid_: event, control, OSC packet (as a distinct internal type).
