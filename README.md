# reuben

A configurable musical instrument built from composable **Operators** that each do
something simple and combine into complex musical behavior. Easy to start with via
ready-made example rigs; deeply customizable once you get the hang of it. Rube Goldberg
machines, for music — hence "reuben."

Music is the primary payload, but the same data (notes, chords, timing, gestures) can
drive anything controllable over time — lights, video, game engines. **OSC is the lingua
franca**, in and out.

This repo is the **engine and its SDK**:

- **`reuben-core`** — the portable engine and its **embed surface**
  ([ADR-0039](docs/adr/0039-engine-in-core-embed-surface.md)): construct from a document, push
  OSC in, pull audio out. No OS dependency; compiles to `wasm32-unknown-unknown` untouched. This
  is what you link against to put reuben inside something else.
- **`reuben-native`** — the `reuben` CLI and its audio/OSC/filesystem host.
- **`reuben-mcp`** — a stdio MCP sidecar, so an agent can author instruments against a live engine
  ([ADR-0044](docs/adr/0044-mcp-stdio-sidecar.md)).
- **`instruments/`** + **`surfaces/`** — the instrument library and the presentation docs over
  their interface pipes.
- **`.claude/skills/`** — the **authoring and developer agent skills** that run inside [Claude
  Code](https://claude.com/claude-code), each grounded on the *live* engine so it can't drift from
  the code. Authoring: **`patcher`** (build/edit an instrument) and **`control-surface`** (author a
  surface doc and project it to TouchOSC). Development: **`create-operator`** (a new DSP operator in
  Rust), **`sync-docs`** (bring the living docs back in sync after a change), and
  **`rust-hot-path-review`** (RT-safety/perf review of an audio-thread diff). The
  [Make your own](#make-your-own) section below walks through them.

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

## Prebuilt binaries

Building from source (below) is the primary path. Each tagged release also ships a prebuilt
`reuben` binary for Linux and Windows on the
[Releases page](https://github.com/Impractical-Instruments/reuben/releases):

- Download the archive for your platform (`…-x86_64-unknown-linux-gnu.tar.gz` /
  `…-x86_64-pc-windows-msvc.zip`) and extract it. Each archive bundles the `reuben` binary
  plus `LICENSE` and `README.md`; a matching `.sha256` sidecar lets you verify the download
  (`sha256sum -c <file>.sha256` on Linux, `Get-FileHash <file>` on Windows).
- It's a headless CLI — run it from a terminal (no installer): `./reuben play` (Linux) or
  `reuben.exe play` (Windows). All the subcommands below apply.

## Quickstart

Run the default synth — opens your default audio device and listens for OSC on UDP
`0.0.0.0:9000`:

```sh
cargo reuben-play
```

`cargo reuben-play` is a workspace alias (defined in [`.cargo/config.toml`](.cargo/config.toml))
for `cargo run -p reuben-native --bin reuben -- play`; anything you add after it is passed to
`play`. The `reuben` binary is subcommand-driven: `play` (live audio), `describe` (list
operators), `validate` (load-check an instrument), `scaffold-operator` (new-operator skeleton).
Add `--help` to any of them. The other subcommands have no alias — run them via
`cargo run -p reuben-native --bin reuben -- <subcommand>` (everything after `--` is passed to
the binary).

`play` opens the default device by default; `play --io-map <file>` loads a **device profile**
([docs/device-profile.md](docs/device-profile.md)) to bind logical channels to a specific
device's channels, pick a non-default device by name, and request sample-rate/buffer-size
preferences.

Play a note by sending OSC `/voicer/notes [midi, gate]` from any OSC source:

- `[69.0, 1.0]` — note-on, A4
- `[69.0, 0.0]` — note-off

Send several `/voicer/notes` messages to play a chord.

## Run the examples

Instruments are **data** — JSON files in [`instruments/`](instruments/). Load one by
passing its path to `play`:

```sh
cargo reuben-play instruments/<name>.json
```

| Rig          | Plays on its own? | What it is                                                         |
|--------------|-------------------|-------------------------------------------------------------------|
| `default`    | needs OSC notes   | Polyphonic synth (8 voices): voicer → osc → filter → ADSR → out. What `play` loads when you give it no file. |
| `groovebox`   | **yes**           | The Groovebox Toy (ADR-0022): a free-running 16-step drum machine — kick/snare/hat synthesized from operators (no samples), each a sequencer driving its own voicer voice on a shared clock. Toggle steps via `/kick_step1/in`..`/kick_step16/in` (also `snare_*`, `hat_*`), ride `/tempo/in`; per-drum volumes (`/kick_vol/in`…), a master DJ-filter sweep (`/tone/in`), and a main volume knob (`/volume/in`, default −6 dB) are Good Buttons, with a warm `saturator` gluing the mix ahead of the filter and a `/drive/in` knob to ride the squash. |
| `chord-player` | needs OSC       | The Chord player Toy (ADR-0022): tap-and-hold diatonic triad buttons (I–vii°) at `/chord/in [degree, gate]`. The `chord` op stacks scale thirds and the voicer resolves them through the tonal context, so held chords re-spell live when you change key (`/key/in`). A 12-voice pad; `/brightness/in` tones the mix. |
| `strum-harp`  | needs OSC         | The Strum harp Toy (ADR-0022): drag-to-strum. Stream `/strum/in [0..1]` and the `strum` op plucks a note each time the bar crosses a string boundary. Strings are scale degrees through the tonal context, so it stays in key. `/octaves/in` sets the span; `/key/in` the key. |
| `euclidean-drums` | **yes**         | A self-playing 4-channel Euclidean rhythm machine — kick/snare/tom/hat synthesized from operators, each driven by a `euclid` generator on a shared 16th-note clock. Reshape patterns via `/<chan>_pulses/in`, `/<chan>_steps/in`, `/<chan>_rotation/in`; per-channel DJ-filter, level, and decay knobs; `/tempo/in`. |
| `mic-space` | needs a **mic**  | Live-input demo (ADR-0038): a top-level input pipe bound to logical input channel 0 feeds the nested `space` patch (`instruments/patches/space.json`) — speak/play into your default input device and hear it through the tone+reverb, broadcast to stereo out. Fails fast if no input device exists; pick a device / remap channels with `play --io-map`. Tweak `/space/tone/in` (Hz), `/space/space/in` (mix). |

The rows marked **yes** make sound immediately — good for a first run with no OSC sender. Every
node's inputs are live over OSC at its address.

(The one-feature example rigs that used to fill this table — echo, reverb, vibrato, metronome,
sampler, and friends — were culled from the library; the ones tests and benches still exercise
live on as frozen fixtures under `crates/*/tests/fixtures/` and
`crates/reuben-core/benches/fixtures/`.)

To play an instrument from a phone/tablet, project its **surface doc** (`surfaces/<name>.json`
— the presentation layer over its interface pipes,
[ADR-0043](docs/adr/0043-surface-docs-decouple-presentation-from-instruments.md)) to a
TouchOSC layout with the `control-surface` skill. A surface doc is a portable presentation
contract, not a TouchOSC file: any host can render one.
(The v1.4-era walkthrough, [docs/v1.4-control-surface-testing.md](docs/v1.4-control-surface-testing.md),
predates surface docs and is kept as history.)

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
| **Make / edit a control surface**     | "make a control surface for this" → **`control-surface`** | Author `surfaces/<name>.json`, then `gen_surface.py emit` |
| **Add a new DSP operator (Rust)**     | "add a wavefolder operator" → **`create-operator`** | `scaffold-operator`, then implement `process`     |
| **Sync the docs after a change**      | "sync the docs" → **`sync-docs`**             | Edit ARCHITECTURE/README by hand                  |

A typical first session, by hand or by skill:

1. **See what's available.** Every operator self-describes its ports and params:
   ```sh
   cargo run -p reuben-native --bin reuben -- describe          # list all operators
   cargo run -p reuben-native --bin reuben -- describe filter   # one operator's ports/params
   ```
   This is the same introspection the `patcher` skill reads ([ADR-0020](docs/adr/0020-introspection-and-patcher-skill.md)).
2. **Patch.** Copy an instrument in `instruments/`, rewire node `inputs` (a literal or a wire-ref `{"from":"/node.port"}`), or ask the
   `patcher` skill for a sound.
3. **Validate before you play** — load + plan with no audio, surfacing errors/warnings:
   ```sh
   cargo run -p reuben-native --bin reuben -- validate instruments/my-rig.json
   ```
4. **Play it** with `cargo reuben-play instruments/my-rig.json` (above).
5. **Play it on a tablet.** Player-facing controls are the instrument's `interface` input
   pipes; presentation lives in a **surface doc** (`surfaces/<name>.json`,
   [ADR-0043](docs/adr/0043-surface-docs-decouple-presentation-from-instruments.md)) — or is
   auto-derived from the pipes when no doc exists. The `control-surface` skill authors the doc
   and projects it to a [TouchOSC](https://hexler.net/touchosc) layout (`.tosc` files land in
   `control-surfaces/`); other hosts render the same doc directly, with no emit step.
   `surfaces/groovebox.json` and `surfaces/euclidean-drums.json` are worked examples.

Need behavior no operator provides? That's a new **Operator** in Rust — `scaffold-operator`
(or the `create-operator` skill) generates the skeleton and wires its registration
([ADR-0021](docs/adr/0021-scaffold-operator-and-create-operator-skill.md)); see
[docs/agents/operator-dev.md](docs/agents/operator-dev.md) for the operator contract.

## Going deeper

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — the design, end to end.
- **[CONTEXT.md](CONTEXT.md)** — the glossary / ubiquitous language. Read this first if a term is unclear.
- **[docs/adr/](docs/adr/)** — the architectural decisions and the reasoning behind them.
- **[docs/agents/authoring.md](docs/agents/authoring.md)** — authoring Instruments and Rigs (the guide for agents and contributors).
- **[docs/agents/operator-dev.md](docs/agents/operator-dev.md)** — building new Operators in Rust.

## License

BSD 3-Clause. See [LICENSE](LICENSE).
