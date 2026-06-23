# The launch Toys (V1.3): three beginner instruments built from Operators

## Context

[ROADMAP.md](../ROADMAP.md) V1.3 ("The Toys") is the payoff of v1: **instant music for a
non-technical person**. The MVP proved the engine spine, V1.1 grew the operator vocabulary, V1.2
settled the playable control domain (Message-first control, the math family, Good Buttons as
composition — [ADR-0017](0017-playable-surface-and-control-domain.md)), and V1.4 shipped *ahead
of* V1.3: the `control-surface` skill generates a TouchOSC `.tosc` from an instrument's `control`
blocks ([ADR-0018](0018-control-surface-generation.md)). So a Toy gets a real, touchable player
surface for free — the remaining question is which Toys, and what they force.

The roadmap named four archetypes — groove box, tap-to-play chord/melody, drag/strum, meta-effects
— and OPEN-QUESTIONS parked the **Toy-design thread** and two **gesture operators** (tap-to-chord,
drag/strum) "once there is UI to drive them." There is now. This ADR settles the concrete slate.

Two facts framed the tree:

- **A Toy is an Instrument, not new format machinery.** Per ADR-0017 the build is "new Operators,
  not new instrument-format machinery." Each Toy is one self-contained Instrument JSON (the unit
  `control-surface` consumes) plus a generated `.tosc`; internally it is a graph of existing +
  a few new Operators. This keeps V1.3 out of the still-ungrilled Rig/nesting/library thread.
- **The generator draws only fader / stepper / button widgets**, in a uniform grid (ADR-0018).
  No XY pad, no multi-touch grid. Any gesture a Toy needs must reduce to those three widgets, or
  pay for a generator extension. This single constraint shaped every gesture decision below.

## Decision

### Three Toys, one each across the distinct gesture modes

Depth over breadth: ship **three** Toys, chosen to cover the three distinct player gestures rather
than to maximize count. Melody-player overlaps the chord-player gesture and meta-effects overlap
the existing fx instruments (`echo`/`reverb`/`djfilter`), so both are deferred; breadth is cheap to
add once the toy-construction pattern is proven.

1. **Groove box** — rhythm / auto gesture (toggle a step grid; it loops).
2. **Chord player** — tap-harmony gesture (tap a button → a chord).
3. **Strum (harp)** — continuous gesture (drag a fader → a glissando).

### 1. Groove box — multi-track synthesized drums

A free-running step-sequenced beatmaker. **Three lanes — kick, snare, hat.** Per lane:
`sequencer` → `voicer`(1) → a drum-synth subgraph → `mul` (lane volume) → mix → master `filter`
(a Good Button) → `out`. One shared `clock`; the pattern loops forever (no transport — instant
sound).

- **Synthesized, not sampled.** Only `blip.wav` exists and there are no drum samples; rather than
  commit binary one-shots, drums are built from Operators (the reuben thesis — everything is a
  graph). Kick = oscillator + a fast pitch-drop envelope (no noise). Snare = noise + a tonal
  component, enveloped. Hat = noise → highpass → short envelope.
- **Steps are toggles.** The `sequencer` gains a `gate_mode`: each `stepN` reads as boolean on/off
  and the emitted pitch is one per-lane `pitch` param defaulting to `0` (root → no sample/pitch
  shift). The generator renders the boolean steps as toggle buttons.
- **16th-note grid.** The `clock` gains a `division` param (gate fires N× per beat) and the
  `sequencer` expands from 8 to 16 steps, so a lane is one bar of 16th notes.
- **Surface:** 48 step toggles (3×16) + tempo + 3 lane volumes + 1 master filter Good Button. A
  flat 48-toggle grid is acceptable — it *is* the step-grid idiom.

### 2. Chord player — tap-to-play diatonic harmony

Seven buttons, the diatonic triads I–vii° of the current key/scale. Tap-and-hold sustains the
triad; release stops. `chord` op → `voicer`(polyphonic) → a pad voice (saw → filter → slow-attack
env) → `out`. A key selector (a `context` op driven by a key/scale stepper) makes held and tapped
chords **re-spell live** on a key change — reuben's signature, and the reason this Toy exists over
a fixed-progression pad.

- **New `chord` operator — degree-in-arg.** A single node, one input address; each button sends
  `[degree, gate]` (the established `/voicer/note [midi,gate]` message shape). The op tracks the
  set of held roots and emits stacked-thirds **degree** Messages, resolved through the tonal
  context (always in key). Param `size` (3 = triad / 4 = seventh). Degree-as-arg (not as a param)
  sidesteps the deferred port-tagged Message routing (the degree rides the arg, one input port)
  **and** future-proofs the parked sequenced chord-progression op — a sequencer can drive the
  degree arg later. The seven-instances-with-`root`-param alternative was rejected: simpler op,
  but `root`-as-param can't be sequenced, a dead-end for the harmony roadmap.
- **Surface:** 7 chord buttons + brightness Good Button + key selector (stepper).

### 3. Strum (harp) — drag-to-strum

One big fader is the strum bar. Dragging it streams position messages (0..1); a new `strum` op
emits a note each time the position crosses a string boundary. Strings = scale degrees via the
context bus (always in key). `strum` op → `voicer`(polyphonic) → a plucked voice (osc → filter →
percussive env) → `out`. An open-scale (full-diatonic) strum reads as a harp glissando and keeps
this Toy gesturally distinct from the chord player's tap-harmony.

- **Fader, not a new widget.** A `strum` op reading a fader's position stream and emitting notes
  on threshold crossings needs **no new widget type** — it reuses the fader the generator already
  draws. This is the parked drag/strum gesture operator. Realistic multi-touch "strings" (a
  drag-sensitive pad row) and an XY pad were rejected for V1.3: both demand generator + layout
  work that belongs to the later reactive auto-UI, not this disposable surface.
- **New `strum` operator.** Input = position (Message, 0..1); params `strings` (count, default 8 =
  one octave) and `range`/`octaves` (string span). Output = degree Messages on each crossing
  (both drag directions strum). Notes are plucks — a percussive envelope rings, no held gate.
- **Surface:** strum fader + brightness Good Button + key selector + octave-range knob.

## Consequences

### Engine work V1.3 forces

All *modifies* are backwards-compatible — defaults preserve current behavior and existing
instruments stay bit-identical. The three new operators are authored test-first via the
`create-operator` skill.

| # | Change | Type |
| --- | --- | --- |
| 1 | `sequencer`: add `gate_mode` (boolean steps) + per-lane `pitch` param; expand 8 → 16 steps (default `length` stays 8) | modify |
| 2 | `noise` operator — white noise source | **new** |
| 3 | `filter`: `mode` param (lowpass default / highpass / bandpass) — the SVF already computes HP & BP internally | modify |
| 4 | `clock`: `division` param (gate fires N× per beat, default 1) — a thin slice of ADR-0006's deferred subdivision | modify |
| 5 | `chord` operator — degree-in-arg → stacked-thirds Messages via context, `size` param, tracks held roots | **new** |
| 6 | `strum` operator — position stream → note per string-crossing via context, `strings`/`range` params | **new** |
| 7 | `control-surface` generator: emit buttons with custom `[degree, gate]` payloads (the chord buttons) | tooling |

### Build order

Operators first (2, 3, 4, 1, 5, 6), each via `create-operator`; then the generator extension (7);
then assemble the three Instrument JSONs via the `patcher` skill; then generate the surfaces via
`control-surface`; then a hands-on TouchOSC proof doc (the V1.2/V1.4 pattern).

### Deferred (explicitly not V1.3)

A 7th-chord toggle (the `chord` `size` control on the surface); clap/tom drum voices; a
chord-locked strum; multi-touch / XY widgets; a single-operator `drum-sequencer`; per-step drum
pitch. Each is a cheap fast-follow once the pattern lands — recorded so they don't compete for
attention now.

### Shared seams

The chord and strum players share the **context + key-selector** pattern and a
synth-voice-behind-a-voicer pattern; the groove box and any future melodic groove box share the
`sequencer` `gate_mode`. The new `chord` and `strum` ops are the two **gesture operators** parked
in OPEN-QUESTIONS, now built because there is UI to drive them.
