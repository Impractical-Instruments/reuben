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
| `default`    | needs OSC notes   | Polyphonic synth (8 voices): voicer → osc → filter → ADSR → out. What `play` loads when you give it no file. [▶ play][default-link] |
| `groovebox`   | **yes**           | The Groovebox Toy (ADR-0022): a free-running 16-step drum machine — kick/snare/hat synthesized from operators (no samples), each a sequencer driving its own voicer voice on a shared clock. Toggle steps via `/kick_step1/in`..`/kick_step16/in` (also `snare_*`, `hat_*`), ride `/tempo/in`; per-drum volumes (`/kick_vol/in`…), a master DJ-filter sweep (`/tone/in`), and a main volume knob (`/volume/in`, default −6 dB) are Good Buttons, with a warm `saturator` gluing the mix ahead of the filter and a `/drive/in` knob to ride the squash. [▶ play][groovebox-link] |
| `chord-player` | needs OSC       | The Chord player Toy (ADR-0022): tap-and-hold diatonic triad buttons (I–vii°) at `/chord/in [degree, gate]`. The `chord` op stacks scale thirds and the voicer resolves them through the tonal context, so held chords re-spell live when you change key (`/key/in`). A 12-voice pad; `/brightness/in` tones the mix. [▶ play][chord-player-link] |
| `strum-harp`  | needs OSC         | The Strum harp Toy (ADR-0022): drag-to-strum. Stream `/strum/in [0..1]` and the `strum` op plucks a note each time the bar crosses a string boundary. Strings are scale degrees through the tonal context, so it stays in key. `/octaves/in` sets the span; `/key/in` the key. [▶ play][strum-harp-link] |
| `euclidean-drums` | **yes**         | A self-playing 4-channel Euclidean rhythm machine — kick/snare/tom/hat synthesized from operators, each driven by a `euclid` generator on a shared 16th-note clock. Reshape patterns via `/<chan>_pulses/in`, `/<chan>_steps/in`, `/<chan>_rotation/in`; per-channel DJ-filter, level, and decay knobs; `/tempo/in`. [▶ play][euclidean-drums-link] |
| `mic-space` | needs a **mic**  | Live-input demo (ADR-0038): a top-level input pipe bound to logical input channel 0 feeds the nested `space` patch (`instruments/patches/space.json`) — speak/play into your default input device and hear it through the tone+reverb, broadcast to stereo out. Fails fast if no input device exists; pick a device / remap channels with `play --io-map`. Tweak `/space/tone/in` (Hz), `/space/space/in` (mix). [▶ play][mic-space-link] |

The rows marked **yes** make sound immediately — good for a first run with no OSC sender. Every
node's inputs are live over OSC at its address.

(The one-feature example rigs that used to fill this table — echo, reverb, vibrato, metronome,
sampler, and friends — were culled from the library; the ones tests and benches still exercise
live on as frozen fixtures under `crates/*/tests/fixtures/`, `crates/reuben-core/benches/fixtures/`,
and `web/bench/fixtures/`.)

To play an instrument from a phone/tablet, project its **surface doc** (`surfaces/<name>.json`
— the presentation layer over its interface pipes,
[ADR-0043](docs/adr/0043-surface-docs-decouple-presentation-from-instruments.md)) to a
TouchOSC layout with the `control-surface` skill; the web player renders the same doc live.
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
   `patcher` skill for a sound. Documents are validated against a JSON Schema generated from
   the operators (`crates/reuben-core/schema/instrument.schema.json`).
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
   `control-surfaces/`); the web player renders the same doc with no emit step.
   `surfaces/groovebox.json` and `surfaces/euclidean-drums.json` are worked examples.

Need behavior no operator provides? That's a new **Operator** in Rust — `scaffold-operator`
(or the `create-operator` skill) generates the skeleton and wires its registration
([ADR-0021](docs/adr/0021-scaffold-operator-and-create-operator-skill.md)); see
[docs/agents/authoring.md](docs/agents/authoring.md) for the operator contract.

## Status

**MVP complete; v1 in progress.** The headless "it makes a sound" spine works end to end.
The signal/value port-form refactor ([ADR-0031](docs/adr/0031-float-resolves-to-value-or-signal-by-wiring.md))
and Voicer-hosts-voice-sub-patches rewrite ([ADR-0032](docs/adr/0032-voicer-hosts-voice-subpatches.md))
have landed: a port is a held **Value** (`f32`) or a **Signal** buffer (`f32_buffer`), read/written
through the contract's typed handles (`io.read(IN_X)` / `io.write(OUT_X)`,
[ADR-0037](docs/adr/0037-typed-port-handles.md)), and polyphony comes from the Voicer hosting voice
sub-patches (`instruments/voices/*.json`) rather than the now-removed Lane model.
General nesting ([ADR-0034](docs/adr/0034-instrument-nesting.md)) has landed end to end: a
`subpatch` node references another instrument (cycle-guarded), inlines into the parent graph at
build (zero runtime cost, internals still OSC-reachable under the node's address prefix), presents
the child's `interface` as its ports — each entry declaring its own type and quantity
metadata (unit/range/default/curve) since the ADR-0038 pipe flip (presentation lives in a
surface doc since format v3, [ADR-0043](docs/adr/0043-surface-docs-decouple-presentation-from-instruments.md)),
type-checked by the ordinary wire check — and
`reuben describe <patch.json>` introspects that boundary (`instruments/patches/space.json`,
nested by `mic-space`, is the worked example). The library resolution story (#122) has landed: a reference resolves relative
to the document that names it (a library patch bundles its private sub-patches and samples next
to itself), falling back to a configurable instrument root (`reuben --instrument-root <DIR>` or
`REUBEN_INSTRUMENT_ROOT`); the resolver canonicalizes source identity, so two spellings of one
path are one cycle-guard/dedup key, and an in-memory `MemoryResolver` serves embedded hosts and
tests with no filesystem. Documents carry a `format_version` (absent means 1; a newer-than-engine
document refuses to load with a clear message) and the document is the save source of truth —
`from_graph` is the explicit flatten/export path ([ADR-0036](docs/adr/0036-instrument-library-and-format-versioning.md)).
The I/O-mapping epic ([ADR-0038](docs/adr/0038-interface-pipes-and-the-device-layer.md), #185) has
landed end to end: **format v2** makes `interface` entries typed named **pipes** (direction
flipped — an input pipe mints an address internal nodes wire from, an output pipe is fed from an
internal port; the old anonymous master `outputs` array dissolved into `interface.outputs`; v1
documents auto-migrate at parse and render bit-identically, and save writes v2). A signal pipe may
bind a **logical channel**, honored only on the top-level played graph — and **audio input
exists**: an input pipe with `channel: k` carries real device audio (`instruments/mic-space.json`
is the demo), the input stream opened only when a patch binds input channels, crossing a
lock-free ring into the render callback with resampling and drift compensation from day one,
under fixed, counted xrun/ring policies surfaced as diagnostics. Logical channels bind to real
hardware outside the patch via the **device profile** (`play --io-map`,
[docs/device-profile.md](docs/device-profile.md); the worked pair is frozen as a test
fixture, `crates/reuben-native/tests/fixtures/stereo-sub.json` + `stereo-sub.io-map.json`). Live input is the one sanctioned
nondeterministic boundary — offline render injects known buffers, so the determinism story is
unchanged (ADR-0038 §10).
The web player epic ([#151](https://github.com/Impractical-Instruments/reuben/issues/151)) is
under way: the `Engine` now lives in `reuben-core` as the shared **embed surface**
([ADR-0039](docs/adr/0039-engine-in-core-embed-surface.md)), and `crates/reuben-web` — a
workspace-detached crate — runs it in the browser as raw C-ABI WASM inside an AudioWorklet
([ADR-0040](docs/adr/0040-raw-c-abi-worklet-boundary.md)): fetch-on-miss resource staging, WAV
decode in WASM, a flat tagged control channel, and mic input over the same duplex seam, with a
co-located ES-module JS API (`crates/reuben-web/js/`) and a plain-Node CI harness that renders
the whole instrument matrix headlessly. On top of that engine the `/web` player app
([ADR-0041](docs/adr/0041-web-player-app-in-repo.md)) is now live — a Toy launcher + shell whose
payload is staged by a transitive-resource discovery script and deployed to Cloudflare Pages — and
as of P5 ([#227](https://github.com/Impractical-Instruments/reuben/issues/227)) it is an
installable, offline-capable PWA: a `vite-plugin-pwa` service worker precaches exactly that staged
payload (wasm, every Toy's transitive resources, surface docs, derived icons), so a home-screen
launch plays with the network off. Share links landed as P6
([ADR-0042](docs/adr/0042-share-links.md)): the ▶ play links in the rig table above encode a
self-contained bundle (document + transitive text resources) into the URL fragment.
**Format v3** ([ADR-0043](docs/adr/0043-surface-docs-decouple-presentation-from-instruments.md), #247)
decoupled presentation from instruments: the per-node `control` block and pipe `label`/`widget`
are retired (a v2 document keeps loading — leftovers are ignored with a `LoadWarning` naming
each; sound is unaffected), an interface pipe carries only the quantity contract
(`type`/`default`/`min`/`max`/`curve`/`unit`), and presentation lives in **surface docs**
(`surfaces/*.json`) that the web player and the TouchOSC generator both render —
`surfaces/<id>.web.json ?? surfaces/<id>.json ??` an auto-derived default from the pipes.
Remaining rung of the epic: the SAB ring (P7).

## Going deeper

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — the design, end to end.
- **[CONTEXT.md](CONTEXT.md)** — the glossary / ubiquitous language. Read this first if a term is unclear.
- **[docs/adr/](docs/adr/)** — the architectural decisions and the reasoning behind them.
- **[docs/agents/authoring.md](docs/agents/authoring.md)** — building Operators and Instruments (for contributors and agents).

## License

BSD 3-Clause. See [LICENSE](LICENSE).

<!-- BEGIN share-links (generated by web/scripts/gen-share-links.mjs) -->
<!-- Play links for the sample-free rigs above: a full playable bundle (document + every
     transitive text resource) encoded into the URL fragment by web/scripts/gen-share-links.mjs.
     Run `node web/scripts/gen-share-links.mjs` to regenerate; `--check` verifies them. -->
[default-link]: https://reuben-web-player.pages.dev/#r1.tVZNb9tGEGXRS6F_0NuAl9gNRTG2ESQyejDioj4lQAzkYrj2ihxKGyx32N0lLSXwf092lyKpD1px0egggDPL2Xlv3hvw7a9B8HUEEOakCmbualSakwyncBLZMJfaqKpAacIphBnmrBImdKmMUhu79DEoSazKBUmeQinYis0EguJzOHoDNfEUdQQXlx_HSXJ6chzDNcoMJi6hJpIMargpeMYjmDODt2DIVYEjjOcx3Lx-GycRvIqTW7CHxyTh4ixax5MunufH56BtcY01KibaSgzSBakshr9YuvAtAdfAYEHaYAb3DbixS92Drmbjkpl0EXu4CjVVKkUdTsEyBi0d_g3LhQc62YjHnzXJcATw2BBqUOXMnW_KUGXKynR1faj3aMejqLA3TKgyMasyTmGTfByt__0FkjLX442LtxXNquxaVA6Si7MsU6i1q72dayZ8RdpoeAMbsDp-UJ8DE4JSZqfoZ8lSRVqDWWABzA6jKtwTV-Caj7tLWvI22fSoov0gPGP7QVA_weUWs_awY68f6vPrOdikeE1yn-zb0ePolyAIfg-CYHDswZ-__Qd7NQzsM5muZkax1PAaGwm7EcBRz1ofJEJBkhoz6pU0jd6nVu0oMsgV_guZ4jW6qQDplAvBDKlofcTa0OVQ1iioxBiuvFHeg-EFaphZT917vu6tBDSBIJZpeOkcp0EbJjMmSCLkpMCgNlzOY2g9MHWdTNxdXEZeG5MGn1V6OOSZncGGttLmoNdqyU9P7mZVnvek3Zk3nMLZWRInvUzB3YC2gmzpgkmyGU8rVbtbcFn2y1eSu5lefWmN2qrS4h1sdX-P-zrc1-CrONmUajS4YzzNA2sGZR03B3ba3zXQ_7eeOiUOuFunO_vpQ_vOeU_Z0grUCtgp9IXuVOcPOQXFT6-KHU31obrk0IYYWFw5F2Zo-27nGnR_ozQCQdBDybQ-0PDTu82S1zW8IWJDeR5O4dTJ-4egrPfCfjAo6x0kF5fXHyElaahSMEeJys_M2f_QzJodcXhmO-bqM-CSz5xZSQ-o7uwS0XwumRgEfOd3wTbs6wUr_SoVXCJT7UaFd58sQAImAZclSZSGMzHWZiUsBaIqEFxNOFr-c3J8APhyELU1c1o_F3dRiR9CXadsB_OndxdTv83hD9CWgKwP-5CKhz3nPbJXxLOn8DezeSYFP_E7Y83cgU-M4PvvGw
[groovebox-link]: https://reuben-web-player.pages.dev/#r1.7Vxfb-O4EQ_QhwJ5b_s68Mvat7bjP0k2myAH7O0eurj20MPtIX3YbgNaoi1eJFInUk5yxQL9EP1C_Sr9Im051B8qsixZ8dnZxd3DIUuLGs7wpx-Hwxn-983BwT8OATpzEQVEXS9pJJngnXOY9nUz41JFcUC56pxDZxEJsaQzcdfBH13h6NZXMI8oHUQx54wvQN5z5VHJfqYuSEXDgaQ_xZQ71IUZJSogNzSC7tV4OIUfxD2M-_DqzfeD0WgyORqNppPeEH7wIkrBJ5xK-Fs8GY2P4YY5N32QnES0Dx5RaTsljgcExqcDLQpmQviUcFgQRSGVG0FX__s6EC69_CNRtAdzSl09VgKC08FSMIfClf5_BEq_3RNSSVCeGcUzCW4UBwPUDMzTIVGOB109rqT_kRle9i-PKPN3H2Yx8xVQrlhE_XuYRyIAEdKIKBHJPnABkgShT2VvCH_hFKRHIuqC4wvnBogCly2ZnpbLY9Dmk6ix8gZcKAqLiLkXONaQKEUjDr4QoYS5iOiSRn3Qc0i4Aili7qI0FREuQxGpIXy9pNE9hD65p9FgThxtFUdwFQkfmATCgXFFozlxKDAexgpCFlLomjk7nvbOQYnFwqc41xJumfLgSJvlWjeMjxgfDq1_nx4xDl3CXThCc2HrF3048ogyf_cuzKzihDKZzangR2I-H8L3zKWgaBCKRBj-fcR4HycLlsKPA5qMRJvFSF8KX4tOpKb_0lKTv2-4mMk-yFtKQ-x36wmfwg1T1quU4FQ__OabwZz5ikbYDbqj4QlEVIOG01hFxL-AGfXFrXldAiVxGxIp9bRK6vaBzMSSgvIYl9rIAh_y2MLTT_X6oI0kqcLmgEgtzKdL6lvDMbqmo4euS-ck9hXo4VzC4BTcr_B7oghgCNidtujCj_XHeA8EJFExwhCIR4kLYm6Le_MNGC0vgCkJR27Elpa08XA4HfUTvYmCaQ8i5lKjrRZikD4jGvUDweGWRIEeuoB57Psgf4qJ9IbwJooDeY7fOFyCZJwaFQnMidSAU443cCMRAuXLC_OZwSVwwSSF50BACU58ENK5QG64zKxI3eQpxZyboWGtiEoRRw6VnXPQ1AfQyT5VTWf4hzzK2oY_SsGxK0An_96tR_PGwrMWHVgPW63m6UOAjwnXJh9aPjD84vKBas7VU2A1AHTUfYgC5tNJIto8abCgqXw4stoDpul9XGwjd_q5UaHRiSMU1aF3of3imDNcDe46SdvH9MdO-hGPNxzheMUIRytGOB6OqmWONhQ62obQvWg62Yem031oerwPTU_2oenpHjTdB472AaN9oGgfINoHhl7sQc-zPej5csd6LoW_sZbTk9Yicw99h3rmQne5dudSN9V1O1J3ybq51F3ybi51l8ybS92Ue7cjdZfsm0ndC5g2xdJWhO4FSrtcxTOhewHSpuv4VoTuciHPhO5yJU9DTxvr-aK1yDystsvlzZK6y7XcErsnbXdJwJbYXS7nlthdkrAldpcLuiV2l0Sci90Pona5pudS94OnXa7qudT9oGmX63oudZcLey5105X98VLbLO3tN-l40rUpdiertFwV_n_58mWhNY3zf_Xdt-VIvz4I26Hm5qDrlxd4aIntiFg9PH4RsSoOQp9w6UEciVgNSewy0Sm-Kzve4cLFM6f32J69MdUCD3szPTrEdSMqJb76wU_Jsfs7c0isWMD4YmifEQ--NCdw-hA8jH1JJRzfQUgjPICH7sPT494wf3npxEkLTF7dOYfjgvnKeLQtYn7Nfvq4ysYlM2SH9qtNof3pkiX-pA8O9dnvOYxPs_NiPI_uA8UDbo8pPOSW5jQRXLrQOQajGs2N5asUxF-H2syWlpZ5sqwD_bROPLBh6lO-UJ5G32nBpji-ElDL7rw9ECt4t3IgK9zy1d1Ha_o3Eb9O_qRB_8ma_tMG_adr-h836H-8pv9Jg_4na_qfNuh_Wt2_gfnWWK-B8dbYroHp1liugeHW2K2B2dZY7UV97xfVvc_qe59V935Z3_vllukRnZLySoEJE6sJ8gJmxLnBdUEkTXCCqSfj6edBjnY8pA07FiIbbeixEKNow4-FaEMbgizEDdowZCEC0IYiC3v5NhxZ2JW3IEl7e92CJe19cguatDe8LXjS3rm2IEp7C9qCKe29ZAuqtDeFLbjS3t1tlyw9okpU-ZaoSqIU8zny5Jny5OdBjXm6VhtitBKv2tCilULVhhStZKg2lGilNbUhRCtBqQ0dWqlGbcjQShpqQYV59k8LIszTeFrQYJ6P04IE88SaFhSYZ8i0IMA81aUF_eU5Ky3IL08-2ZD6ML90zR76elmmviznPc9nvbBz4DGeMGcRlcCURYCO4HO2KBKgSXatCOasYkwdjJCVdsBtf9kEVngoSbK18nNb2yeJ7K0xkJXD-1QsZDz_Jiay05Jb2wgzlNZYKMuefir20Yt9E-vkqeBrbRNM5LqPS_jXTtk87wIhlCeLRQlJPcVNGsNK6hfg_ag__tAzhQEEXl9BN5hIuIQl8WM6-FKyBSd-XeQOQ55r9qA6fL2SllLvwwzZ9j8UwzjsaDiaNGKiIPav59PJtRnwGqORICxZDK2CUVX4IjXM66sapUmNzsUwbVHvWa299MRuSMaVYMnOEGrRkj2ZwEVmW_oqvLRHRn6y8QSgYQazChvGBNsDR6J2O3QUZnJb8EjSQGvBkTyXQMNLtjDbB0aalvoEYKGHsgoUWvntQQIVbgcIa-42hANx3Vr1A3Z3TVazZZf04HlSI9Wd1S0QNVypTdzqSyh23KbeJa1x6XyOYnuJ9hoF3VlPH4RhoVla_NbaGMbeLSDQwg6VdIClX7VkgE-lq0RW3Wda34_709GHYsXeFnjB1KT9wqyQ6VKxTqwI5vyVRAEowj1TjCg4WqiAiAsIKbkZcF397OuS5T5IAWJJI5-EoS6G9XTlo6lUxErapErS8Zn5vRtRhR6tfrfwXV09a-ptdUFlJEQAKmJBT5c56jnIKySTqdK1lHXARAZaA87Vxi_XCpZmrchPVl9TqmkO0R8HW50pUIta_VAC2qz2tFBhi6uYhVxT4ZoU29a6yX0thUNE-IK6ebFtVt8KoZBMMcHbfwSYENH6G5g2szEJaykyFPI6IOWVES2MVtS0-OabTGV4Pxj3xx_OiybtV9Yv54XLebVyXTg0rST9M-OURLYJaixaiU8Rq-sse-NhO750UJHWUbKq-6MBQcWSg3AsmfPbBxXSyBkmOGCIirq6dPxCXzBgEJxYMq0C7_p0rnq6Fjo1InQjtvBUr5_Ogca5Q3VV8KP4QRa2vnbIOEFAZc8US9tawYxHVksGieOW0gHjqSu3hglMrfs2VrMk1egJOLna135oqIQdUx_Xts7rK3R4mK6Fx4sC2rs7BvMb-jv57G6IF5NntdoGwv5hc-wXtyvF0RwCfDj8ePibg4OD3x8cHKwu5j_492HVDSmTfvmGFCv-2D98kKGEPoe5HSS5PgNvO3llh8U0W1C-pL4I8QIKidM6E8oDfQGC-UBMClMXL0qYCfe-DzOCNyG41CH3eAeL5pqeuUHCI2HicOg7RpiKXQr_-ee_sMXxiZTMMXe2wDPlxUH4bAjfoQAmzd0fGk9dLvQtLz89vA2kZ-J7-r6UZzLdduI9KoL798btlFk22jDvfm601rd24AweEUdpb0in0XX6ja9DwEO0Xzzp00bvqqxAM_aKxEDKl8PkgfR9OdJL4N1eQmEKo9XfFeVlEtZXz2QYeW5QloFx8CX4uHrD66u-uZgD4Zb4tdKANIcn3tehAZggr46MlCJ4ZDoajkbjwsQ55B7b7dbSvNuWqz5ZjahPiUyI-sw-homlIikUHrfI4bd3TYLyfuR1ujMwN5qYm2ZIIGKuoPv250bB3Mmk8QAbrDDhKhi8c4hv3LvCCGf32NY1KOjlyDDrjv4t458cAu3DcZkZN1yG9Pe2xSCcptbqmcSQC7KvZkedbXDfeCqPzxrOZJPIiBa_OiCEtH1pBmnCI8Vl4lFRIrTOhhOEoNvURZAO8_3q7b-QThnH-fqYTw6uSDwFc361UI0F0LxVCuGPK41wS5ZU-w3pcDqNlA3FLY1qZ9xsqh4q_fVdKLi-aYz4A6nu_SwMjI9D9-7vk14aCSFBWO8p3237Q2vi-Drl4ObV61fJFVF6SuEL49i4zZSoBrAGzob4NYZ_Qj6uQ-p83D_k_u3D-6cOxhs5uPbpse3hmhzT9i5u4qaiE0GSS7tworULQUB6IlLJPV_o90aML9D3ZBHIONDOanYy_6sD-6sD69Iq19VASXISDuGdhTqDOfRYBaePd1cL_uU2_NXTR_mrn_WignO3WqMHPxXoKqeZYU26R4NFC192Xbl0GVnbWraMVrtYuNbH9efVrrEh66JbrC8lzXn6mUQab-Irj89OGuK8jaf4Aw7VEYGBuQmt9CDmbhLTRXM_xkVMjLVFJ7EJJFFqJSLxROBT86PanhIXLw1Fz8JshgxMIzzKe8RGKP_6N7RCNkdPx6EsHiiudCh_lzuUxYtSD3787SbuZJ5uZzuTb9lAH-HXOJKpV5it8xrJ5sj2we2vyalAQBXxfeaY22B_dRI_Bydx45VfV3gYVJhrgGtW_nVHg175lPVtepaHJ8yM56tHCYQeUY861lvjAjixEvN55xxejEYPJj05wHpbuNFXX4nMCXeMozluxD-be-dXuhDbfLI1TjrGE7Mbm6vt08AJP96yEz79JJxwzZ6fVmTHyz7LbXkk3sapa59gYOfg___9Dw
[chord-player-link]: https://reuben-web-player.pages.dev/#r1.zVldbhvJEWbeAr3lAkGBQGJylzOkSMX20rABLSXEwmrXguUICARFbM0UOb3q6R5391CiFw58h809kjPkKH7OIZL-GXKGMzSlxT5Eb-zuqa7-quqrH_3nd63WT3sA7ZmQKdHXC5SKCt4ew6hnlilXWuYpct0eQztKhIyDjJElyrbdj0VkNi72wxG8E0sYwudP_wCdIEzMWXBnoXN49DYYDIbD_mAwGr4A9_NgBBnNUHVDOMcFcrjJtRZc2a-skJgSLTiNQEtKYgUnnz_9vKD03_8CMbMHolxK5BpucTkGTbKA8DhIBItB5UoTyhUQsGr3QCJDohCUFpkCqkM4JlHiLwWFPFZW5tSen1rdoNO3v_qUd-EyxrlE7MGcaLyCjpccSCE0qIgwBHcCvrZHui8q8kQGREPfP0VpEt0q91kgkRFNFwg6oTJWQHgMmFKtYOokToELjaoHdwmNEit2IWiEEiQqwRZoVJcin7s9LThhEAmu8V5bkygBCbLYStYkyzB2yit4exycnx2fnsLpycUx3CXIYSlyiBLC52il3eISOgYf95xbXDpwetC_xWWfcrg8eB6Gz4ZXXY-pt87-0GmpgBo7JEJpjD0e3o0Ce2AKKr8JMqKjBDqK3EHwChQTdwHRBiY4PDp_CxeTQ4-of7rKU2ux1L6KwPRG0nmiOSq1st56yeo5CMP9q67BB52xU3q_Qo5ASpRGCTPKNMoQzgRbZolxwHHpLZFYmDMilw5SZXxXJx7PEM4kKuSaaCo4MGpsQzmoXM5IhKpffn34oxI8dKFkDJnLCFV7DCYkoRpvDigTbE6Nfn3TSmvvAXz0savR3rkWSHmW6_UFAO01PqVVgLZeZvay2Who1fPLMc5IzgwZDMI_ldZTyu3aoLxG7ttj2K-s5ZxaJvlD2699LDbdY5uVML5f_-AWl4_U-emGglbpg-cNWj8bhoPiwr3StW2R600IRa6resykSI0efZHrkOQxFe2qrJWJuIitvS_t-kpi8RAHyerBJI6lM1Tbs8h6zxPxO5IFWgQJkanghhIfwWcm5ky0YRx4GqqSTwhTRT_gFEbw0hEydDy23R4cwMsiEkL4QYBXAazLrbLCJmm5C1TP8JPnI4mBypAxENwyjyOiEN4lWAQRxCJ6ouCZ598ibaChHstTBGb0HmOwzOw5eRCGT6GzyiDdCmEabMI1mrU4AWgrrJq5bGhnqNXWx5I_GchMOm32p5rJPWzNRq9tFlavUf4XsAYyN3lRh_Dd8V_h_Pj0ePLuzdvxBrdDZ5Pbe_D9ydEJ2HzchVhaYpsahKegBWhJuMqEwheOFf-4kWZWZjWMGMKRcxyYQEp-FHIH9uaWreAbHlhD_yCUHTTNIG_ueYxfC6WVyQJ13l0nLxMkR87dbMiAUXKV8lcQFJ5XGPRFo72-BsKYiIjGIstFUii1PuvqhCILUgmWa8JSqnq5PU2tHxgJPqPzKuDue0PfJSL8ookK39xmpWK_MUgsWFs_9VxXt_FKoVVubMiYX3QEl-mbHWFzzzvC965GYOIuI0oZltKWmdIUY1NOvIAo12I2czXPOZ2buHQsaIOGw40rbtepF_4sRAzfWhrbEQkunWxDyrlQNedUkXa6bRWQkuzaH3lkSKVDtSWetKTpNhQTJLEUIh2bMl1wpQnXgO9zwoJM3KE0eSaFzt_3--q91J39YbcLKhVCJxgD5VoAgcmFTR8EZjljsD_0QWldwWS0pQJjDtsuMJpBhJRRPjelNEMgoJcZjQiD0edPPz-1cVv5lok83mEUX_wMn3_zMKhydj0bDa-V9Y2t-b0OWqU8dd4GHdKFrxxMkwvo3HR3OdBW23t_b3Sbm-0uZzV9pLO4Kqr55aK88fgQ-CXqbPXddYheR4vGtHv2l29PTyb11uOJcqy7ICxHuBz09q98D1VtSZrcuSCNDuEwTYdqaqjEsr4RFrxyfmPiZYFSo-z2zC4HaQolL8f2NmV38azk6iib4SWajEa0KeUf4uDNeJcaiEbPSUVsMT63zyzX5Zqm6JqG0cOsRLKdYVPir01rlejWmSN4VTD15Wgw6H0zGAyu4PWHHuB9JjhyTY0NMpQRZjonjC3BpNBdARblcmH1PV6LKT_7QVgad2uEU-T62rUoRt9K52K3rPSR2am1HFd7H_d-02q1ft9qtb7cQ7b--dttY6HhjrGQT7qV4ZClY9fcz4SEWgNcjIdGw24IbzhCKrhwfTeYYcDZ4ZEraEyWsFE1k_i-KEGNpwsVUcaIFrJXHDE9jcvMG3OEEH54U8REghJXvUlzPi6uIaAon7PNqKqlf-iYMQdkQukgpfddH3AkdkOMjlEHnDp9P5Hqmvi-IbcYm2hcBbp7mpH2REEf-cJW91IwBbnC2BTdCrX1UJ_yMiJJqoBI5E-0_TLwwUFuGJopRCERY_-JCU_WDWE1LBhbcPsWPsp7rqbsk8iOp0xD2374cMFI2tqiX9_ks1mpvqp06gcHTa36sGnAMBxsBsIqAvE-K4svhg-vP9SHCea9j56A1DXcMgF56CjBwbxlmoB8EfoDNfXrOfHXm0Ksg2tLwlZRjWzPyZ31eTNog46r_WmkbPzXU5Px5IRkuItaa_5UKV_MZiNn3pEFGiLzirWbrFF7NfIFMpFh85uR18uB8xLRmLcXEqDDKEciYXLRdbOWuGgBfL2-o96xIq1zPa04YkSWdnVUWq35cRkiu9kIkWciK-55eXrhpuhu_UG42bJ9Z55Gvrh2YVpD0TiCBcejtoJxcuFLJF7O0IHSS2aQZHlq_x-wQOjc_224y5fut6Jk4qySfn-1ot68ehGR2psvJodjm-m-cnEQlx_9i-t5E5ePLObXZvn_KegL0LZp5Cub1v_-_gs
[strum-harp-link]: https://reuben-web-player.pages.dev/#r1.zVjNbuPIEVZugY8551DgZaREoijJ63E0wACznsHOHjYbjA1fBo7VIotSj8lubndTsjYYIA-Rd8gD5A3yIgHyJEl1kyIpUrYMbID4ZHU3u-vnq6qv6l-_6fX-cgbgxVKlzNxvUGkuhTeH2ZCWudBG5SkK483Bs_-P1kxlnt2NZEjLN3IHM5AxzKB_O_Fn8OpmjXAjd_rVEN69_zQKgukU_vn3GYyLn7PpYA4MIsVWIyNH9l6ge334USAs-QpiFqECrsGsEdyBJVPw77_-jX4hS2FsV8dcwOfA9yd30KejC7u6AC4MqpiFCBnPECLFN1ysio9SVANgIoL6FzKDLMnDBw0MhDQIyMI1GJ6iPZZJzQ2XAkIltUY6pY2iO5cyFxFTO-gvpVlDxBWGdFI7uQc-XNuDGphC0CFLECJcKUQNCrVMNkiSKJmv1vYpIwVLIJTC4KMZgpbABMgMBcR5kowizowUPCyuUsgiDYwEIhPCKuFaMxFJq6E2bKeBC3jAnQ8fSCUZ21cuYSN5iJqMzGAttcGoMIb18chuL0Dny1HGTLiGvrUPRqDZFkZvIUMV5lrzDcK799ef4Pbq3eCNvdt-qkDnqXVgakVhsFR8tTYCtYbvpIzg29wYKUhfdI5O-ePeEgxSpg0qiHliUPlwtWZi5ZzxgDtI6N0tN2sYP-COcND_4fv338P5pe9fBAOQiqy7Zpn7xBnLnV8zlUqxG-vA96sfFz5cbxEzGFdy1uBlpPOBBUNhCJIc-g05IcyNjGP4vfWtYCJEQgCaEskEGZ0xUcgiQ8M26B6a-P75ne-Ci77OVYjamwOFKNTjz_mGgs_5cHy45X_RUnhnAF-LOC6iobqMiyw31eXl9bUFAM_sMvtKPJtaqYrlCGOWJ5QTAj-oradctNfYozeHiR8US1_LPa_Q_IUvTjpenHS8eN7x4gPuXvjaRZeC55cd79HJ1oMVkF5s14uX2vWs9rQnc3PoXpmbphCxkuRubyxz47M84tJr3rWHj5CR9dNnu76_sdTCAWevNIsi5TT29um22i2LRiOty8yHRZlhF9BnsMYkgluW5GhzflE3JoN2SeBGYxIPIcYIljt4ogj0D0vG4I1L8ktKTzavU3Duq4BL0j58Qm00MEMesEneyLSI5AFlZyFdwYi5QnsukSzy4bI4o0FuUMEEHOBJyMLRLlkuikBYlFJWKWEAWx6hKFTOmPArQ7YC-EhINTxd7O83v9YQVdr_6NfOz53fFpp6c2jGxgYTGXKzOw7VFpqKfNyNp9ZmAacrSNkXqYYwmY5uPtz4UOHroNTWKi3XtriSbW3lpEKYbKt6WbjnAXd717hKM4RGpdEGM-ciJaUBZpEEVBoKj_evZr5_dT6gGmIUEzqTGofNOqQv2uXqGW8XmnlzeN0wOklx1ImUA7tdGLRyjJ54c5g2l6Yut9aXZt4cvmkunbeE0t94c_hDc-mCYHEqLhyn6IbF4V6Bio-SAvcSDqtjxWlQ-_DemrEM-0OYlE5yFCZJZMgMlj7ecx0N_Uwmu2xNxMyiimI-YVlWyyi29Bu5QrNGR2VZxda6mRNXYDNzDQmhFDFfNZHgZHDR1ypCXdAp4-gYSsr9TqRQsjueY_Ypv_q2JdKevrQozZMQcAyrGwKHewUEfmhwM-nomyYBI2KbbzromqXp13xFJNzZzrYPKMry8m0njX0mWF2BPWY05_ZmFW4a3cl59IKUZffFkc7P99o9eYNq1IaTwtIxjW6fyPrGy41SuPSFAqVT3S1NRcbuw00LKDWnXt26-rGovnAF4JV2vGRjecnnYDi5A51KadYYEdmQwODqtsROnwlYpFO9KDmL_Wz0VrvtUIoNKoNqMKRdATETwvYUrgncSlDU8ESQskzDEhO5pUZCRN0NSoOnXJSFrSQzCovua89PnvYMP04DaqS2E2qpjKwjrq1l6vyWmmlXZGanuZJl9_Fseu8s1u3VGvAPXVprN52vRm_LcP98EQTDySQIgjv4-PMQ8DGTAoXhLHnGMGGuNla4D9UndR1PMhwBsNN2Mjf3BbMn4Rrl0u7Z6y-C4MSieaoFKfBPMV-VJD8H_mQY-K_vhpBwgUz9EoA6wS6B_7rTJoE_aXUvd2dfz37V6_V-2-v1nmqUe__49bE52PTJOVhRtOrTsFtLMNzAJJaqxj3sU9Dfj8DcsCuVQjraAPXxir2YhmQ24cQKf3JFyGUSqUOeJMwQ4y2OrIiW2Ig_mMr0yTkj0oAJin7Dwoch6LVUBiIM2W4IiRQrUJgg067dsl2RFQeKDiY3Ax_--GNZSNeo3El68chYp5CXAXVVCTbHOe1SDH2aQdGczYxS_jjYc3NH2biGL7k2pDoBEcWG5k1-1ePNrZXG1g5E0W1tGbPQkCWoxfVOH4XQTUc79vtlHsc1qtFo3M_PuyYG067efRocRvg-t-BjVr8-F9wC7-PP-w59H2uk7_9-aFNPMl3DBWfmI_MFFBu_ONASv80Afrm5RBUlR-iJDltJ75ptn85jLWg0GAttdiawLdsgJZfyjZOyNwrqnzPsFh9Fm8X8qQp-G75zeD76hQSda8O4OBr7FK3caJBb4cN3zFSjFkdc39RmMjJ7pSFjXGFkxyIjqrexbcH3HzzHla2YDpXBtIHhkO1aaG2FQN0ldvMIJbYJz153WW9LnTFeMLLI5BbVs3UWxebeRXgLdPu235VSKB1PbNLxSlGnKCNtdgmZMslTBHsn9B__PB08Y9fHo1aiEG3U3tPYRZ6cpPUmZC2db6_ezW2l-x3YqUdUV_o5fBwfcOmw29nLp1Qv3PL_0_mURjsmUcFtev_9-w8
[euclidean-drums-link]: https://reuben-web-player.pages.dev/#r1.zVxfc9u4Eb_nfIodvZwUS7JkOzmfU99MmriTu17bzCWTmc71asMkJCImARYAZfs699H63Tq7ICXKgiiSkmz7ySJBYLH_uL_dBf_38Ztv_vsCoDNROmH2csa1EUp2zuC4j5eFNFZnCZe2cwYdngWxCDmTg1BnienQkFAFeO8tGB5PBmnM7oWcwskgiJiUPIaL4iHQ0b2NEkhYEAnJoXsjgps-GMk074NVSR8iZntD-IfkYCKmeQhBrIIb6IZiJpCu8xMY_AAMxq9tNJDKcphqEfYg1GLGDUxUpuHKUXkFUy65ZlZp8wY4CyIwqeYsNCCsgas0iw03VxDhLz7jMr4HFmhlDFwZy1NzBd1bYSO40soyK5S8AqvARGJiwUYc0ogZ3gMmQ-AJTsJgyiwHGzELE6G5cf_mfPjWADINZkoEfAgXM67v3Q8QBsy9tBE34ncewkSrBFSa0w5dqcCwJI256fVpiRCM1UxMIwtqMiFi3J4dAbG44XQx4VYrqRIOgwEgs-EcDHL-WoX3cAAhD5ywUmGDaBBqlebigHOQShgOB2CVZDFoIackIzgHBtdKJYJriMQ04hqcHHGv53QpZcbwMJ_BiuBmCB-5LhgB9yqDKbdnJc0IlLRaxQa6xPpDJ5zDgvO9PjB4f_Hu7T_hRqpr6OLuWJIClzMeq5Qjc3EzuFEliaBDJFZYZBW3QcQNXCsbEV8sEzHJjcSIewdzy3lKy1yLVMVMw_uf4C8__vz54pd8ycEYYnWLW4NQ3UoYDmGEUqL_DsbzjUNG88gQGPx88eXiZ3p-CJ8j7vQzZ4MBkyVgI62yaQQMEmYs12C1SFDPkDSV2TSzha4QGcIAkyCk5XrCUHVkmllIRcqh-_b9L4PR6OT4DaSaGy4d7yAm0xASTEbPmMMHZjz8apTsnYEWIQfLk1TBTDA4pH8PhXwDmpuIpRxSZi3X0rj7f8Kd_HCZy0rI_vySE2L5SiFJmo6YjYrFlyaaiNhyfSgk_DoYD4fj396A4RZilYWSm-VFYz7jMQ0dLUaSXMujSCcOhXTcN4lSNkIDcwvRHPDuiyEWod4g04m3qPLLZF0GMxLqEgF4UaqQm5J4y0uTYEg1YzSqkE9YFlsYDUejMWhm0XhsxNzCmluBLk_IGG00FpZrFuPgM7ofKxaiFSmuAz4IYpakPHQuphib603JKjSTUw6TWCndB6PypX_85PQrDoFPJjywuH_NjYUZizMOXeZUqiBYZdagcqCXyzcT5lOjQhJpwLVWugf_yo5G4xO4FnYgQi6tsPc0yBiuLW0PinfN8aXmt1pYPtRm2MnfN7lmd84A30t0Kc2smf8G6JBili7gpfsUH-lMjo9oovxyvoHOGYyPRsNR6U4i8CU3Xr7G7jpn8P333y9dzaSgl9-fP_6tk1_9o7jdQU-TW0BDgk485Iw85Ixfe6nJ1_QTRPbXlEGva_JnDUFuTT89hfU3JMknMS-LXjWmiMyzOTmjsY-g5atE0hqK1lDjXMwOuDPwCmw48i9LPqzxqqev6gnFsyrFFu3M5Wg_5uIoekb24ghqaTC1fUojg3EkPRuLceQ8vsm4ddvZzOvWJmNV0s5gjvdjMEjPMzIXJKelsdT2KI2MBQl6NqaCxDy-oeCqj20mEbPtzOR0P2aC9DwjM0FynlUQhgQ9GzNBYh7fTHDVdmbSOAB7UVq547IKS4BKZXaZCExBIRGHKrNDloVCdZbnepHP1yH42zmDX-n6AqLlu6D83XwfHRaGmhtDUz-4lacRP7m8nxWJkNMhLCf-ELRSisuZHZzcQco1XHNmEbM-zAq69BsuQxk-fZ-nyYZ5lgMTHoYmdbkOAr3DBUkryBPJzAlaibZWQWmZj-7u_NYfPsmsMM9R6-ceBfA8W-HfXzHNt0iszXOgQ7jojl_3T_qjHpxTpmKg5MBGfEDJAYL8yNScUSdAhtvbwA0nw3WbprtDFFhp5yWWeRx2-fEytvY-7_VoKzPMR3nnWHXSKxMsubDasstzMFXSkzO_-FiSHri0ZDFLkfe9vi9ne4k3Q7giR3pVVucFxs1zg3jVJVDzVGvvDGIlp1zD-TydS_mzt39_X9xxNGBaeIMaMGsZ6cFDR7zq4le464Z4ZUOaU_kwzyrUS_OYM8MdWadlmWfGsuLNWkuayZGpECTlMy9ZYlfE-U5JY5m0eYbdJT5ZojJpofvh903mRTQeYeaqHpVZfDk5Pro0YipZXEFw6lO9T2XiXs5VD959KVzvcr2glmJssKw547wCvN4gfDkbBrOmhlktymtUmLVSpBoKDoGJ5v_JuAzua4vx5LSmFFkY1pMi0rBCK5GId9CuGVVw5mIjdm8nMeJPG2GRyjUUlTKBiGN8e1VwQZlgVZWLIle_JCiqEcrChS5KXhsYQlyu3BqN8PLkls04JrsLmjq1tp2qW67rqUCQ6dmqvl7cpUpi6p3FA2PvYw4zFWcJBxoO3bt_H_WKcgdW0d592cCDu70YYl1vNQvYyha_vHt7lpsj1jJfAhWownrb2aDjqFFtVNwJY6fuaF57WlVxKmaVX_dubP6-f__ToPitjMDwp19U1XpY4lLA4HeRplwPJprzzTwTG6KsHD95-ZaokC9oLqMaKxL3fh7XYlb4NV9mPcfCryus-sj1vBnh_U959a_Q_xsqTxdMeuAk5hVDKuFuUCpCSZU8QkX2B8L58nVYfLlkaUvhjlGSycAx9OjVDtSvKHLW0D5XTHXKR2Xvfl6anavb1irmwHJbDRsd7dYrsST1v3xJEeAlFOXl7ZxR-LWVL5pLrqE7qoKcLgHuwZyfqGVkPeg86p8g6ERluWbBjYPrhKrgFRXVx8dPCjSXylJ-29qENB-UbbyTVEPNciWqqdAqsWYuNl_ET3Ir99E0xZml0tA6oEkdRSnEXE5ttEnMLYFkuULlZX0lkiz0ui6ULGepm0PJetGdo-nJw7u57rSI76j9q2p3DwYsKaXrHcPYbtjZ2l-XllsfTLoVdxVIlnfY0H-XRb-rYNLNiR1Xl5P18Na1-i1DW2zWcbEQToEdRZiXrfEiH5--qmkRm2CeI96H8z4TwYFKnEFAF7sbe5DJkDuiSQDbALwlxnkF2Q7j1VdbWnut1lIT3W6VtgX82UJl62Q73PSJuFvhwXKzat7WuuhY3YX5ro_aN_FjLrndWnFNTFge_ASgcKlLw8u-R0KFeWzQEBbSU1shvoXS-kPSTZDvobDXxCW7xHylDpea6rVf1FfuuGmrQjuEfY4cH-5zbmhnwG-usq3czj6gH7Z0eIDfZ5VUwL7j_pGDfehL6JiFO4BRHLNweBD7kvFMBfZlKw0MDTGGCefxU0LCUtuV3_A2AcKlxiTvFNVwcNFn1VSMlWCQBOmBgijJ7cuO8-an51x1XHRoecVSCRWdHdQEiuOjrYDiWjdNqtG65IhHZKZc2rh89oCyoHWC-p3WIsnI2pUirUqKMz87Kkguc7Wh583NapflSJyyuhqJB5w8xUgUcH4wq7l8j-vKt07cTu2HviIlUr6XGuWcaS3kt48KJc77pAXKuQi8DNlnfRJXfvL81RZ2WdN_VSDzZLe1yUKXWqj2bpNJi67ijShhMfQJIGip-9nLs0cCoBRvNISfViVbgc9CM_2h7ybouSzgNXHOLoHnvGO8lkLtF3QuutfbKs0OIScS4wOc6Fx2BjdzBW3hV_YBNbEt2gM1PzBbATVPXVtr-Vg8VRlPUXG6rqVV0XFbxFRPWmwsnVTwW9YmZLnUy--dohpZLo4mNJRaRUEH5_SXc1BuLjtrVKYDvqGcU-VIcZFo1RY-FAfv6fSzkIvEf-EOEm5ZHIsAP1GwlVtdbNPL9yCzajLpnMF3o9GDjvzca3xIO2udaL13TTW8J-PxAKovaAAmUtrSZxq2KfnOT25UYHy3En75AT-OQB9JqMH6dnB-cZLEK5NKOO98Td267_FWcL5e1IwUbR81o4y3iJpzLdpT1Iyzr42ao7m72FXYnHuNhm-3hRx2FTUvDhltDHIWQ58gai4dhnrKqJnMumHUvAv33j5qXhbwI0TN8wNktRRqv1Hz4jDbM4iakRhf1Eyvvl1FzbmCtvArbaPmOim3RNxdrrpW6ks8qFfUrO4rRLY23PKibLan_V574dFBHXdQiW5a7LXQvP3sdGWf9AW1A2JwDw6ga1VygJ9Qwyz5JItjuBEWEnHXmg1OnRoywclkV-9N_CCWz8HN0-DlD2d1I85CrVSC3zpSM65jlqaYVqbyY6jkt3ggVGxEgPmh2u925pKQupUt5JQXLol24EocxTa2EVxjQJ8zuqHg3HFi_65V-UaLLBYybR05LwB-e_HHi2_yv_8D
[mic-space-link]: https://reuben-web-player.pages.dev/#r1.pVfLjuNEFA0bkHrJFxx5M4kmcdwPNMi94a1ZIEDMiA2NxhXXdVwzdpVVVU7SPWqJj2DJnv_gU_gSqCrbcTppMQxZJHK97q1z7jnX-eyjyeTtGRAVStfMvtqQNkLJKMXl3A0Laaxua5I2ShHVIl-YhuUU-Umucjf6rdjQQsimteBUK0w__-rHRZJcfjpLwWBVs6hoQxXCkkY0hJVqJYdVqNRa5Kyfy0smJVVIUBBxA1sSJBlLHJmPm6FhNi8x3edlln6IzNKviF8bJWf469ffoCShEBXBlGor5BorZUuUrNqQgSr86T6bWnGqYmS1yDMIAyYhpCVdsJxGWacQFrWQ1mBZixxChgQVJ0hWk48PJvkcK8pZawi2FAZrzZrSndtU7JY4mHWgwIMyh7AGWXfxFEmGlZDcZauJcfMoQNNb1WpwKlhbWfTob0ROTwwKoc2w-BqaatZgK2yJzOWAxUKoRc2abA6ucrMMGxeNVg6vuOazGC9LgmlXAe9cSdPWZBwCzAOkNBeS6VtshSZMs7c3UaFVfROluIkcPDfRfRZ4ENIITgOZc-yJemKgthKZkFmgQhgUxGFLrdp16feYW2lLMuKOOBwjc0jakMbqFgw1W4scJdN8yzQ5zFx19AWIP_-47K6y0orxnBkL1dqhDqc98hlULawlPoMh2ZXeliyMWEtWuVL11WMsaVI9tCbGDw5PYVNkmtoVSU8yxuU5iMbfOAs8MHl7QBoaTYakxdQXX78btmTWV4Q5rACDggn_bSy2JUlIdXgg7YSxxhMwguPZzBUoTEPszbJL1Sp_21rk19h6ut2jU-CK5W9CncZ4UbKGUIkNQTn4v3_xZYqguaVVkpZCYpq3VhXFHM_vZvN-Mny7We14W6EWuzmSOD6fxXDe0eXthAfDZG6FksQhleRkSddCCmNFHkzDldyY3_OkE3tRVEK6CpCcNIR8Tbk1eCNdfa3aoiBtPLDEeBzsS5NRrc7JRCmcBwJRMLcU0bGnRGfAfeeJnTfs9_kb7M-Bt8rRIxDZ28afXFxevAr5-Cy62Y7WKEXSDd773_uwJgpFexBBtfYwgtOfi9Al7eYPzxpu4CzLnfWzHx9O7FPsZT8kGDHONRmzP30_1TWBl3urdtXwtOOaioJyex1czql8IHEsd5e5L7p9s3AG65bE-0hHIPuxg-cxCo6BYeJ-hHVPchJ_Mhp1Wbu2lyRJPHAwxA54pP3mAc9fzu7PPphMJh9PJpPjmpn8_uF_aa_HrfU7MpatKvKY4mlwTu9QHbKDFq6WQ8et1LZhxgRlM3yjyXMxB-0aZUbuum9yDmkD45aXytiuy7ZmcHwX3lVNjC86AtPA55S1XKjZHJlL0T3DbKmxCFbgT3azXfueOpZDbSxqscNPrGpD-BmEvEamWtsfOj-sjbBr5vw7xlfCePeq2Ioqs9wKviZrgj0J6YykDTfjKp9DKgslu1t2iF25zvC1axej9xJOecW0v3XoTE4S3jFJroWkBclC6Zw4NJNrGjnRTZsk7NnFDDftRXJ-1cPxhqgJzaQQlSXdweJ6dFtVuEji-CJJkgTP78KRc9QOe_cCsHXe77Zuzvey6RO47nuyNqEFe5yEPWrFLg_fizvXeyf3eiCrU-Z1JJFeVqe3jc2ue2vxErwca7D2cZM4GY-xXZTi_IQmO8W-s8fuw155kR8FvjgV2dNzMJ63euPD0K4Zn99K4WX8_O6B776Ph7taj70M3tPEQ72dtvCHc72HO5XnSlqtqhRrkraiwU1YSYz3r81Bi__izSH7x-xZyNPuHATy6DZP-n7jKaCPoAjZnobicKYDordMWCaqa_fHYOf_GLir90p8YgZT85f_X2AEQg75PoSlFrtHd4960sNtWvk1SfzsqIpc65r88_kb
<!-- END share-links -->
