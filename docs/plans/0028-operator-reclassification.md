# Operator reclassification table — ADR-0028 Phase 2

The mechanical contract for the operator sweep (plan step 8). For every operator, each
port/param is mapped to its ADR-0028 **shape** and **role**:

- **Float in** — a number read per-sample (`io.signal`) or block-rate (`io.value`); materialized
  from a latch when unwired. Absorbs the old "signal port + same-named unwired param" pair.
- **Enum in** — a held, live-switchable named choice (`io.enum_index` → the op's generated type).
  Absorbs an old discrete-valued param (a `mode`/`type` keyed on integer thresholds).
- **Note in/out** — pitch/velocity-ish events, read via `io.events` / written via `io.emit`
  (the old Message ports on the musical ops). The Message carrier survives *as* the Note shape.
- **Harmony in/out** — tonal context, read via `io.harmony` / published via `io.publish_harmony`
  (renamed from `io.context`/`io.publish_context`).
- **Constant** — instantiate-time structural value. Only `voicer.voices` today (drives
  `LaneRule::FromParam`). Kept as a param slot until the `config { voices }` grammar lands
  (deferred until the voicer needs it); the voicer's `process` never reads it, so no `io.param`.
- **Float out** — `io.signal_mut`.

`process` rewrite is uniform: `io.param(slot)` → `io.value(IN_x)` (block-rate) or per-sample
`io.signal(IN_x)`; `io.input(p)` → `io.signal(p)`; `io.context` → `io.harmony`;
`io.publish_context` → `io.publish_harmony`. No operator may reference `io.input`/`io.param`/
`PortKind` at Gate 2.

| operator | port / param | → shape | role | notes |
|---|---|---|---|---|
| **oscillator** ✅ | freq | Float | in | already migrated (P0) |
| | waveform | **Enum** {Sine, Saw} | in | Float→Enum in P2a |
| | audio | Float | out | |
| **filter** | audio | Float | in | bare wire-in |
| | cutoff | Float | in | was signal+param |
| | resonance | Float | in | was signal+param |
| | mode (0/1/2) | **Enum** {Lp, Hp, Bp} | in | |
| | audio | Float | out | |
| **envelope** | gate | Float | in | |
| | attack/decay/sustain/release | Float | in | were params |
| | cv | Float | out | |
| **lfo** | rate/depth/center | Float | in | were params |
| | out | Float | out | |
| **noise** | out | Float | out | no inputs |
| **output** | audio | Float | in/out | passthrough |
| **pan** | audio | Float | in | |
| | pan | Float | in | was signal+param |
| | left/right | Float | out | |
| **power** | x | Float | in | |
| | exponent | Float | in | was param |
| | out | Float | out | |
| **delay** | audio | Float | in | |
| | time/feedback/mix | Float | in | were params |
| | audio | Float | out | |
| **reverb** | audio | Float | in | |
| | room/damp/mix | Float | in | were params |
| | audio | Float | out | |
| **djfilter** | audio | Float | in | |
| | position | Float | in | continuous knob — stays Float, not Enum |
| | resonance/lp_start/lp_end/hp_start/hp_end | Float | in | were params |
| | audio | Float | out | |
| **sample** | freq | Float | in | |
| | gate | Float | in | |
| | root/gain/start | Float | in | were params |
| | channel (−1..31) | Float | in | integer selector, rounded — stays Float |
| | audio | Float | out | resource `sample` unchanged |
| **add** | a/b | Float | in | identity 0.0 |
| | out | Float | out | |
| **mul** | a/b | Float | in | identity 1.0 |
| | out | Float | out | |
| **clock** | sync | **Note** | in | reset events via `io.events` |
| | tempo/division | Float | in | read block-rate `io.value` |
| | phase/gate | Float | out | |
| **chord** | set | **Note** | in | `[degree, gate]` events |
| | size (3/4) | **Enum** {Triad, Seventh} | in | |
| | degrees | **Note** | out | |
| **sequencer** | clock | Float | in | edge-detected gate signal |
| | length/step1..16/pitch | Float | in | were params |
| | gate_mode (0/1) | **Enum** {Degree, Gate} | in | |
| | degrees | **Note** | out | |
| **snap** | notes | **Note** | in | |
| | ctx | **Harmony** | in | |
| | target (0/1/2) | **Enum** {Scale, Chord, ChordThenScale} | in | |
| | direction (0/1/2) | **Enum** {Nearest, Up, Down} | in | |
| | degrees | **Note** | out | |
| **strum** | position | **Note** | in | fader position events |
| | strings/octaves/velocity | Float | in | were params |
| | degrees | **Note** | out | |
| **voicer** | notes | **Note** | in | |
| | ctx | **Harmony** | in | |
| | voices | **Constant** | — | param slot for now (LaneRule); no `io.param` in process |
| | freq/gate | Float | out | |
| **context** | set | **Note** | in | `chord [tag, ...]` events |
| | root/degrees/s0..s11 | Float | in | were params |
| | ctx | **Harmony** | out | publish via `io.publish_harmony` |
| **osc_out** | in | **Note** | in | forwarded verbatim to outbound |
| **map** | in/out | **Float** | in/out | reframed: per-sample shaper (was Message) |
| | in_min/in_max/out_min/out_max | Float | in | were params |
| | curve (0/1) | **Enum** {Linear, Exponential} | in | |
| **m2s** | — | — | — | **removed**; `snap` is the engine default (materialize) |
| **slew** (new) | in | Float | in | reimpl of m2s slew: rate-limited shaper |
| | rate | Float | in | |
| | out | Float | out | |
| **glide** (new) | in | Float | in | reimpl of m2s glide: timed ramp shaper |
| | time | Float | in | |
| | out | Float | out | |
| **smooth** (new) | in | Float | in | reimpl of m2s smooth: one-pole |
| | time | Float | in | |
| | out | Float | out | |
| **differentiate** | in | Float | in | reframed: per-sample Δ/Δt (was Message) |
| | out | Float | out | event-rate velocity is now explicit S&H |
| **integrate** | in | Float | in | reframed: per-sample Σ·Δt (was Message) |
| | out | Float | out | |

## Sweep batches (each a green sub-commit; golden re-blessed)

- **P2a** — enum delivery infra + `filter` + `oscillator.waveform`→Enum (the proof).
- **P2b** — mechanical Float ops: envelope, lfo, noise, output, pan, power.
- **P2c** — Float ops with state/resource: delay, djfilter, reverb, sample, add, mul.
- **P2d** — musical event ops (Note/Harmony): clock, chord, sequencer, snap, strum, voicer, context, osc_out.
- **P2x** — Float-shaper reframes: m2s→slew/glide/smooth, map, differentiate, integrate.

## Watch items

- `clock.sync`, `chord.set`, `context.set` carry *control* events, not pitch — but the Note shape
  is the only event carrier; they read `io.events`. (The shape names the carrier, not a key claim.)
- `djfilter.position`, `sample.channel`, `strum.strings`: integer-ish but **continuous/structural**,
  not a closed named set → stay **Float**, not Enum.
- `differentiate`/`integrate` semantic shift (event-rate → per-sample): confirm no shipped
  instrument depends on the old event-rate gesture-velocity before changing (plan risk item).
