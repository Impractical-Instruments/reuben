# reuben

A configurable musical instrument built from composable **Operators** that each do
something simple and combine into complex musical behavior. Easy to start with via
ready-made example rigs; deeply customizable once you get the hang of it. Rube Goldberg
machines, for music — hence "reuben."

Music is the primary payload, but the same data (notes, chords, timing, gestures) can
drive anything controllable over time — lights, video, game engines. **OSC is the lingua
franca**, in and out.

## Prerequisites

- **Rust** (stable) — install via [rustup](https://rustup.rs).
- **Linux:** ALSA development headers for audio output:
  ```sh
  sudo apt-get install libasound2-dev      # Debian/Ubuntu
  ```
  (Windows needs nothing extra — audio goes through WASAPI.)
- **Optional:** an OSC sender to play notes — [TouchOSC](https://hexler.net/touchosc),
  Max/Pd, or any script that can send a UDP OSC message. Some example rigs play on their
  own and need no sender.

## Quickstart

Run the default synth — opens your default audio device and listens for OSC on UDP
`0.0.0.0:9000`:

```sh
cargo run -p reuben-native --bin reuben -- play
```

The `reuben` binary is subcommand-driven: `play` (live audio), `describe` (list operators),
`validate` (load-check an instrument), `scaffold-operator` (new-operator skeleton). Add
`--help` to any of them. Everything after `--` is passed to the binary.

Play a note by sending OSC `/voicer/note [midi, gate]` from any OSC source:

- `[69.0, 1.0]` — note-on, A4
- `[69.0, 0.0]` — note-off

Send several `/voicer/note` messages to play a chord.

## Run the examples

Instruments are **data** — JSON files in [`instruments/`](instruments/). Load one by
passing its path to `play`:

```sh
cargo run -p reuben-native --bin reuben -- play instruments/<name>.json
```

| Rig          | Plays on its own? | What it is                                                         |
|--------------|-------------------|-------------------------------------------------------------------|
| `default`    | needs OSC notes   | Polyphonic synth (8 voices): voicer → osc → filter → ADSR → out.  |
| `metronome`  | **yes**           | A click on every beat from the Clock. `/clock/tempo` to change.   |
| `echo`       | needs OSC notes   | The synth with a feedback delay. Tweak `/delay/{time,feedback,mix}`. |
| `vibrato`    | **yes**           | Self-playing drone; an LFO sweeps the pitch. Tweak `/lfo/{rate,depth,center}`. |
| `reverb`     | needs OSC notes   | The synth with a mono Freeverb. Tweak `/reverb/{room,damp,mix}`.   |
| `sequence`   | **yes**           | A clock-driven step melody; the sequencer walks an 8-step degree pattern into the synth. `/sequencer/step1`..`step8`, `/sequencer/length`, `/clock/tempo`. |
| `scale-demo` | **yes**           | `sequence` resolved through a tonal context set to C minor — the same degree pattern re-spells live. Change key with `/context/root`, reshape with `/context/s0`..`s6`. |
| `autotune`   | needs OSC notes   | Play any pitch at `/snap/note [midi, gate]`; it snaps to the nearest scale tone. Set the key on `/context`, snap mode on `/snap/{target,direction}`. |
| `sampler`    | needs OSC notes   | One-shot trigger sampler: a note fires `samples/blip.wav`; pitch shifts the playback rate. `/sample/{root,gain,start,channel}`. |
| `sampler-arp` | **yes**          | A self-playing sample arpeggio: a clock-driven sequencer fires `samples/blip.wav` through a major arpeggio. `/clock/tempo`, `/sequencer/step1`..`step6`, `/sequencer/length`. |
| `good-button` | needs OSC notes   | The synth with one **Good Button** (ADR-0017): sweep `/brightness [0..1]` — a single knob fanned to filter cutoff *and* resonance, each over its own range. Built from `map` + `m2s` operators, no format change. |
| `auto-filter` | needs OSC notes   | The synth with a base-plus-LFO auto-wah: a Signal `add` sums a base cutoff CV with an LFO wobble into the filter. `/cutoff [Hz]`, `/lfo/{rate,depth}`. |
| `djfilter-demo` | **yes**         | Self-playing saw arpeggio through a DJ-mixer filter knob. One bipolar control: `/filter_knob [-1..1]` — 0 = open, CCW sweeps a low-pass down, CW sweeps a high-pass up (zipper-free via an `m2s` smoother). `/clock/tempo`, `/djfilter/resonance`. |

`metronome`, `vibrato`, `sequence`, `scale-demo`, `sampler-arp`, and `djfilter-demo` make sound
immediately — good for a first run with no OSC sender. Every node's params are live over OSC at
its address (e.g. `/delay/time`).

See **[docs/v1.2-playable-surface-testing.md](docs/v1.2-playable-surface-testing.md)** for a
step-by-step OSC walkthrough of the V1.2 control surface (Good Buttons, the math operators,
and the Message→Signal converter), and
**[docs/v1.4-control-surface-testing.md](docs/v1.4-control-surface-testing.md)** for playing an
instrument from a phone/tablet via a generated TouchOSC layout (the `control-surface` skill).

### Offline (no audio device)

Render a tone straight to a WAV file:

```sh
cargo run -p reuben-core --example first_sound    # writes first_sound.wav
```

## Make your own

Once an example sounds good, the next step is your own. Instruments are just JSON graphs of
operators, so you can author them by hand — but reuben ships **agent skills** that do the
introspect-draft-validate loop for you. They run inside [Claude
Code](https://claude.com/claude-code): open this repo in Claude Code and ask in plain
language; the matching skill triggers on its own. Each skill is grounded on the *live* engine
(it reads the real operator set and validates against the real load path), so it can't drift
from the code.

| Want to…                              | Ask Claude Code (skill)                       | Or do it by hand                                  |
|---------------------------------------|-----------------------------------------------|---------------------------------------------------|
| **Build / edit an instrument**        | "build a plucky bass" → **`patcher`**         | Edit JSON in `instruments/`, then `validate` it   |
| **Make a TouchOSC control surface**   | "make a control surface for this" → **`control-surface`** | Add `control` blocks, hand-write the `.tosc` |
| **Add a new DSP operator (Rust)**     | "add a wavefolder operator" → **`create-operator`** | `scaffold-operator`, then implement `process`     |
| **Sync the docs after a change**      | "sync the docs" → **`sync-docs`**             | Edit ROADMAP/ARCHITECTURE/README by hand          |

A typical first session, by hand or by skill:

1. **See what's available.** Every operator self-describes its ports and params:
   ```sh
   cargo run -p reuben-native --bin reuben -- describe          # list all operators
   cargo run -p reuben-native --bin reuben -- describe filter   # one operator's ports/params
   ```
   This is the same introspection the `patcher` skill reads ([ADR-0020](docs/adr/0020-introspection-and-patcher-skill.md)).
2. **Patch.** Copy an instrument in `instruments/`, rewire nodes/connections, or ask the
   `patcher` skill for a sound. Documents are validated against a JSON Schema generated from
   the operators (`crates/reuben-core/schema/instrument.schema.json`).
3. **Validate before you play** — load + plan with no audio, surfacing errors/warnings:
   ```sh
   cargo run -p reuben-native --bin reuben -- validate instruments/my-rig.json
   ```
4. **Play it** with `play instruments/my-rig.json` (above).
5. **Play it on a tablet.** Annotate player-facing nodes with a `control` block and generate a
   [TouchOSC](https://hexler.net/touchosc) surface with the `control-surface` skill
   ([ADR-0018](docs/adr/0018-control-surface-generation.md)); `.tosc` layouts land in
   `control-surfaces/`. `control-surfaces/good-button.tosc` and `djfilter-demo.tosc` are worked
   examples.

Need behavior no operator provides? That's a new **Operator** in Rust — `scaffold-operator`
(or the `create-operator` skill) generates the skeleton and wires its registration
([ADR-0021](docs/adr/0021-scaffold-operator-and-create-operator-skill.md)); see
[docs/agents/authoring.md](docs/agents/authoring.md) for the operator contract.

## Status

**MVP complete; v1 in progress.** The headless "it makes a sound" spine works end to end.
See [ROADMAP.md](ROADMAP.md) for what's done and what's next.

## Going deeper

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — the design, end to end.
- **[CONTEXT.md](CONTEXT.md)** — the glossary / ubiquitous language. Read this first if a term is unclear.
- **[ROADMAP.md](ROADMAP.md)** — what's MVP, v1, later, someday, and never.
- **[docs/adr/](docs/adr/)** — the architectural decisions and the reasoning behind them.
- **[docs/OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md)** — the design backlog: decisions not yet made.
- **[docs/agents/authoring.md](docs/agents/authoring.md)** — building Operators and Instruments (for contributors and agents).

## License

BSD 3-Clause. See [LICENSE](LICENSE).
