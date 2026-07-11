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
[docs/agents/operator-dev.md](docs/agents/operator-dev.md) for the operator contract.

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
`NormalizedDoc::from_graph` is the explicit flatten/export path
([ADR-0036](docs/adr/0036-instrument-library-and-format-versioning.md), as amended by
[ADR-0047](docs/adr/0047-normalization-is-a-type.md): the version gate and the parse-time
migrations live in `format/normalize.rs` behind the `NormalizedDoc` type, minted only by
`NormalizedDoc::from_json` — so "migrated exactly once" is compiler-enforced, not re-checked).
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
self-contained bundle (document + transitive text resources + the curated surface doc, so a
link renders the same UI as the launcher — the 2026-07-10 amendment) into the URL fragment.
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
- **[docs/agents/authoring.md](docs/agents/authoring.md)** — authoring Instruments and Rigs (the guide for agents and contributors).
- **[docs/agents/operator-dev.md](docs/agents/operator-dev.md)** — building new Operators in Rust.

## License

BSD 3-Clause. See [LICENSE](LICENSE).

<!-- BEGIN share-links (generated by web/scripts/gen-share-links.mjs) -->
<!-- Play links for the sample-free rigs above: a full playable bundle (document + every
     transitive text resource) encoded into the URL fragment by web/scripts/gen-share-links.mjs.
     Run `node web/scripts/gen-share-links.mjs` to regenerate; `--check` verifies them. -->
[default-link]: https://reuben-web-player.pages.dev/#r1.tVZNb9tGEGXRS6F_0NuAl9gNRTG2ESQyejDioj4lQAzkYrj2ihxKGyx32N0lLSXwf092lyKpD1px0egggDPL2Xlv3hvw7a9B8HUEEOakCmbualSakwyncBLZMJfaqKpAacIphBnmrBImdKmMUhu79DEoSazKBUmeQinYis0EguJzOHoDNfEUdQQXlx_HSXJ6chzDNcoMJi6hJpIMargpeMYjmDODt2DIVYEjjOcx3Lx-GycRvIqTW7CHxyTh4ixax5MunufH56BtcY01KibaSgzSBakshr9YuvAtAdfAYEHaYAb3DbixS92Drmbjkpl0EXu4CjVVKkUdTsEyBi0d_g3LhQc62YjHnzXJcATw2BBqUOXMnW_KUGXKynR1faj3aMejqLA3TKgyMasyTmGTfByt__0FkjLX442LtxXNquxaVA6Si7MsU6i1q72dayZ8RdpoeAMbsDp-UJ8DE4JSZqfoZ8lSRVqDWWABzA6jKtwTV-Caj7tLWvI22fSoov0gPGP7QVA_weUWs_awY68f6vPrOdikeE1yn-zb0ePolyAIfg-CYHDswZ-__Qd7NQzsM5muZkax1PAaGwm7EcBRz1ofJEJBkhoz6pU0jd6nVu0oMsgV_guZ4jW6qQDplAvBDKlofcTa0OVQ1iioxBiuvFHeg-EFaphZT917vu6tBDSBIJZpeOkcp0EbJjMmSCLkpMCgNlzOY2g9MHWdTNxdXEZeG5MGn1V6OOSZncGGttLmoNdqyU9P7mZVnvek3Zk3nMLZWRInvUzB3YC2gmzpgkmyGU8rVbtbcFn2y1eSu5lefWmN2qrS4h1sdX-P-zrc1-CrONmUajS4YzzNA2sGZR03B3ba3zXQ_7eeOiUOuFunO_vpQ_vOeU_Z0grUCtgp9IXuVOcPOQXFT6-KHU31obrk0IYYWFw5F2Zo-27nGnR_ozQCQdBDybQ-0PDTu82S1zW8IWJDeR5O4dTJ-4egrPfCfjAo6x0kF5fXHyElaahSMEeJys_M2f_QzJodcXhmO-bqM-CSz5xZSQ-o7uwS0XwumRgEfOd3wTbs6wUr_SoVXCJT7UaFd58sQAImAZclSZSGMzHWZiUsBaIqEFxNOFr-c3J8APhyELU1c1o_F3dRiR9CXadsB_OndxdTv83hD9CWgKwP-5CKhz3nPbJXxLOn8DezeSYFP_E7Y83cgU-M4PvvGw
[groovebox-link]: https://reuben-web-player.pages.dev/#r1.7V1bb-S2FXbRhwL73svjwbzsTHZmPJqxvV4bGyDZDWokDRrUgfuQpgZH4owUS6QiUmM7RYD-iPyh_pX-kRYkdaEszkii57IJ8rIY6_aRh5--c0Sew_3f-6Ojf70A6C1oEiF-u8IJCyjpXcBsKA4HhPEkjTDhvQvoLRNKV3hOH3rypEddcfQTWCQYj5KUkIAsgT0S7mMW_IA9YBzHI4a_TzFxsQdzjHiE7nAC_RtnPIOv6SM4Q_jk_d9Gk8l0ejyZzKaDMXztJxhDiAhm8I90OnFO4C5w74bACErwEHzE8-MYuT4gcM5GAgrmlIYYEVgijiHHTaAv_r6NqIff_hlxPIAFxp5oKwJK8GhFAxfDjfg3AS6e7lPGGXBfteIlAy9Jo5HsGairY8RdH_qiXdn9x6p5xV8-4ur3EOZpEHLAhAcJDh9hkdAIaIwTxGnChkAoMBTFIWaDMfyVYGA-SrAHbkjdO0AcvGAViGF5ewLCfEz2mPsjQjmGZRJ4l7KtMeIcJwRCSmMGC5rgFU6GIMYQEQ6MpsSTaDxBhMU04WP4bIWTR4hD9IiT0QK5wiouJTyhIQQMEIGAcJwskIshIHHKIQ5iDH01ZiezwQVwulyGWI41g_uA-3AszHIrDjjHARmPtb_PjgMCfUQ8OJbmkkc_GsKxj7j6PbhUoyoHNGDFmFJyTBeLMfwt8DBwHMU0A5O_jwMylIMFKxqmEc5aIsyi0Fc0FNAZav6XQM1-3xE6Z0Ng9xjH8r57n4YY7gKuPYpTgsXF7z8fLYKQ40TeBv3J-BQSLEhDcMoTFF7CHIf0Xj0uoxK9jxFjYlgZ9oaA5nSFgfsBYcLIVF7kB0tfXDUYgjASw1wejhATYCFe4VBrjupr3nroe3iB0pCDaM5bGJ2B96l8n7AkMETBg7DoMkzFy_gICBjiqaQhIB8jD-hCh3v_OaheXkLAGRx7SbDS0JzxeDYZZv1GHGYDSAIPq94KEMX0ORKsH1EC9yiJRNMpLNIwBPZ9ipg_hvdJGrEL-Y7DW2ABwaqLCBaICcJx1x95CY0Bk9Wles3gLRAaMAyvAAGnBIVAmXspteFtYUXsZVfxwL0bK9VKMKNp4mLWuwAhfQC94lUVciZ_sOPi2Pg7Rom8FaBXvu_apeXByrWaHGgXa0fV1S8Afsy0NnvRyobJN65sqNBcMQTaAYAef4wlwGI2zaDVlYoLQsrHE-14FAh5d6rH0IO4blI56KaJhOrhh1h_cEoC6Q0eetmxH_OTvfwldjq20DG0cGJooTOerMecdASdbAP0ID2dHqKns0P09OQQPT09RE_PDtDTQ_DoEDQ6BIsOQaJDcOj1Afp5foB-vtlzP1c07NzL2ak1ZBmh77GfJeg-fXeJ2rWv20Hdp-qWqPvU3RJ1n8pbonbV3u2g7lN9C9SDkKkrl7YCehAq7dOLF6AHIVJXP74V0H068gJ0n548n3rq3M_X1pDltNo-3ZuGuk9frsEeqLf7FGANdp_uXIPdpwhrsPt06BrsPoW4hD0Mo_bp00vUw_Bpn169RD0Mm_bp10vUfTr2ErWrZ38-qo1rt_9IlytdXbk7NfXSNP3_5s2bytF8nv_Tr76sz_SLhbA99lwtdO0e8IUG26Mpf7r8QlNebYRY4RKNOKYpH6PUC2iv-qxieYdQT645fSOPF0_MeyEXe4t-9JDnJZgx-egnp7Jl92u1SMyDKCDLsb5GPPpYrcCJRfA4DRlmcPIAMU7kAjz0n64eD8blw2srTgIwe3TvAk4q5qvzUbeIOluc-tFk45oZikV7sylEPF2zxBdi4VCs_V6Ac1asF8v16CFgucDtB1wucjO1mggeXoocg0lDz5Xl13VQnh0LM2u91MxTZB2Iq0XigU7TEJMl9wX7zio2le2rEbUezusN0SbvjA0xhOXm2ycb7m8Dvwl_2uL-6Yb7Zy3un224_6TF_Scb7j9tcf_phvvPWtx_tv7-FubbYL0Wxttguxam22C5FobbYLcWZttgtdfNd79ef_d5893n6-9-03z3my3LowxK6p5CJkyYBfIS5si9k36BZofgVKaeOLNfhjjq8yE26liZ2bCRx8ochY0-VmYbbASyMm9go5CVGQAbiax8y9toZOWr3EIk9c9rC5XUv5MtZFL_4LXQSf3L1UIo9U9QC6XUvyUtpFL_KLTQSv3rbrti6SNek8orxNcKJV0spE6ec5_9MqSxTNeyEUYt8cpGFrUUKhtR1JKhbCRRS2uyEUQtQclGDrVUIxsx1JKGLKSwzP6xEMIyjcdCBst8HAsRLBNrLCSwzJCxEMAy1cVC_sqcFQvxK5NPOkqfzC_d8A19u6pLX5HzXuazXuo58HI-YREkmEHANQF0KVkEy6oAqmTXNZM5JsUUkxFsrR3kZ3_dBNr0UJZkq-XnWtsnm9nbYCAth_dDsZCK_NuYSE9LtraRzFDaYKEie_pDsY9w9m2sU6aCb7RNNGWbXi4a3rp181xHlHKfVYsSsnqKu3wOK6tfgG8mQ-fbgSoMQPDuBvrRlMFbWKEwxaOPWbAkKGyauZNTnhu-QcX0tVGW8uhDNVmPP3gg52En48m0lRJFaXi7mE1vVYM3GA1Fcc1i0ipyVhU-yg3z7qah06ihz9Vp2mq_5432EgPbUYzXkqVYQ2hkS3FlRhdWfNKv44s9M8qVjQ-AGqoxJm4oE2yPHFm37dhRGclt0SNLA20kR3ZdRg0_-4TZPjHytNQPgBaiKSZSiM5vjxKyw3aE0MauIx2Q5zV2PwoebpFZLftoAK-yGqn-vMlBNGilMLHVm1C9cZv9rvVaus5XEnaQ9V6woD8fiIUwWWiWF79ZG0PZ24ICFnZYKwey9KtRDORVuZcoqvvU0W-c4WzybbVibwu6oGrSdqwKRV_W-AnDZM7fURIBR8RXxYiUSAtVGHEJMUZ3IyKqn0NRsjwERoGucBKiOBbFsL6ofFSVirKSNquSdMNAne8nmMuIVjybhp6onlX1tqKgMqE0Ap4E0UCUOYoxKCsks6EStZRNxJQKtIGcZuPXawVro1bVJ-1eVaqpFtGfR1uRKdDIWnFRRtqi9rRSYSu9mMZcVeGaFds2hslDgUIgQWSJvbLYtqhvhZiygAeU2L8EMiHC-h2YtbMxihslMqbsNkJ1zygtLK0oZPH950WX4ZuRM3S-vaiadLi2frksXC6rlZumQ_NK0r8EBKNEN0GDRdfyk6b8tsjeeHpcPnS0Jq2jZlXvO0WCNS5H0rFmzi-fVEhLzVCTA0qosCdKxy_FBgOKwZkl8yrwfogXfCBqoXMjQj8Jlj4fDPMxEDx3sagKfpY-sMqnrz5lnDFg7Z05l7blwVRE1igGWeCWy0FA8lBugxKoWvdteLMs1egDCHJFrP3UUJk65jGubp13NzLgCUQtvNwowD7cUZzvGO-Uo9uRLyrPymwDqp_ozv3q50q1NS8Avn3x44vfHh0d_eHo6MhczH_0nxfrdkiZDus7pGjzj8MXTzKUZMyhdgfJts-Qu518ok-LCbXAZIVDGssNKJgc1jnlPogNENQLolKY-nKjhDn1HocwR3InBA-76FHuwSK0ZqB2kPBRnAUcYo-RgKcehv_--yd5xA0RY4Gr9myBl9xPo_jlGL6SAAFTe38IPvUJFbu8fP90N5CBmt8T-6W8ZPlnp9xHhZLwUYWdrMhGG5e3X6hei1075AgeI5eLaEik0fWGrbdDkItoO0_61NlrygpUbV-TGIjJapxdkD-vZHqNvNtLKMxpZH6vMKmLsNh6puDIK8WygoyjjyGU3hve3QzVxhySbllcyxRJS3rK_ToEATPmNYkR50gumU7Gk4lTGTgXPcrj-tHauOuWW7-ymuAQI5YJ9bm-DJMyjnIqPM_JyXfvFkX175F3-ZeB2tFE7TSDIpoSDv2rH1pN5k6nrRvYwsPEJhpcuyhU4V2lhfNHeayvWDAomaH8jjhX6E9JAfvpuMKMHd2QeN-2OAknpHX9SMopF6m-Qh1FtsFj66E8OW85km1mRgS8eUJIyvZb1Ug1PVJ1E8-aJZLW6ThAknRdQwTmBmG4_vOfMrfO49I_loMjPRLJyVxuLdRgAWnedR2SJ41GuEcrLOKGvDm9Vp2N6T1OGkdcfVQ97fRnDzElYqcxFI4YfwyLaWB5OfQf_jkd5DMhKIqbI-WHbb9obQJftz65efPuk2yLKDGk8JEKbLx2nVhPYEGcjvxVhv-AYlwXNcW4fyzj26f7Tx05nQJcffVYj3BVjql9iJuFqTKIQNmmXXKgRQiBgPk04dk-XzLuTQKylLFnkABLIxGsFivzvwawvwawHl4XuioqMYLiMVxrrFOckxErJfj54WolvtxGvHr2rHj1F-1U5NiZe_TkVEWuSpkZN6R7tHBa8mG3a12XwtqW21K92ofj2jyvv1gfGiuxrobFYlPSUqdfMinjbWJl5_y0Jc9tIsWvZVNdGimaq6mVAaTEy-Z0pbmfEyJmxtpikNiGkhJ1LSPlisDPLY6yXSWubhoqIwv1MaRomsilvGd8CJVvf0crFGP04QSU1QVFY0D5-zKgrG6UevTd77qEk2W6nR5MXgUjsYTfEEjmUWHh5wWT1ZLtk91fs1WBCHMUhoGrdoP9NUj8JQSJnT2_qPBQrFDbADd4_k1Lg359lfUqX8uTK8wBKb1HjYQ-4s9a1tsQArgpp4tF7wJeTyZPBj1bwLqq7OgrtkQmiLgq0HRa6U_36PxGFGKrV7YhSJfzicWOzevt0yIIP9lyED77WQThQj1_XjM7fvFabisi8Tunrv0MJ3aOjo6OfvPTnzKXy1LpSjSf6xh87pP_xSHb7d-gsfOAeOIGtZFD0ZEQzXEow-bq8fvAW2KJsEBevnz81Gz5M7UtC-oP_sIxPTVGCYpG6n8bKM8vE5rG-RNbYk5NmNPdYs5MmLPdYp6YME92i3lqwjzdLeaZCfNst5ivTZivd4t5bsI83y3mGxPmm91iOhOjKEx2jGqWoh1rkWMUI2fHauQY5cjZsR45RkFydqxIjlGSnB1rkmMUJWc3qqTvcVFHvbahsFZQ2Ixq4vC1DYU7oZo4fG1D4U6oJg5f21C4E6qJw9c2FO6EauLwtQ2FO6GaXOu1jWvthGpyrtc2zrUTqsm9Xtu4106oRgd7beVgu-Ga5Wnn-mR0stdWTrYbrlGirNxsN1yjSFk52m64RpmycrXdcI1CZeVsW-CW26bUQa9suFwUpzchmmh8ZcPi1ogmAl_Z8Lc1oom6VzbMbY1oIu2VDWdbI5roemXD1taIJp96ZeNTWyOa_OmVjT9tjWjypVc2vrQ1otGPXln50faYZtnZqe4Y_eeVlf9sj2mUHivf2R7TKD5WfrM9plF-rHxme0yjAFn5y0bMYqOROqIsi7nRT3WaQy43qjA4f7neb__sfK8Dg5kQf8ZzVU1u_anvq8c7PVOWntYf-XXlcKcnZjVw9WfePDlhfmq2OPF_
[chord-player-link]: https://reuben-web-player.pages.dev/#r1.zVnvbhvHEWfQL4G_9QWKAYHWZMI7UqRiOzJswJaEWogTG5YroDBUcXk35G10t3ve3aNEFy78Dsl7pM_QN-gr-HMfopjdPfLIO5pSGgP9yN292dnfzPzmD__z-1br73cA2lOpMmYu5qg0l6J9AKMeLXOhjSoyFKZ9AO0okSoO8pQtULXtfiwj2jjbC0fwWi5gCB8__AwmQTiks-DOQufJ0atgMBgO-4PBaPgQ3M_9EeQ8R90N4RTnKGBSGCOFtl9ZITFnRgoegVGcxRpOPn74ac75v_4JcmoPRIVSKAxc4uIADMsDJuIgkWkMutCGcaGBgVW7BwpTZBpBG5lr4CaEYxYl_lLQKGJtZY7t-bHVDTp9-6vPRRfexDhTiD2YMYPn0PGSAyWlAR2xFMGdgK_tke7DNXkyB2ag75-iDYsutfssUJgyw-cIJuEq1sBEDJhxo2HsJI5BSIO6B1cJjxIrdi55hAoUapnOkVRXspi5PSMFSyGSwuC1sSbREhJMYyvZsDzH2Cmv4dVxcPry-PlzeH5ydgxXCQpYyAKihIkZWmmXuIAO4eOec4kLB04P-pe46HMBb_YfhOH94XnXY-qtszd0WmrgZIdEaoOxx8O7UWAPjEEXkyBnJkqgo9kVBI9Bp_IqYIZggidHp6_g7PCJR9Q_XReZtVhmX8VgPFF8lhiBWi-tt1qyeg7CcO-8S_igM3bGr5fIMciYNqhgylODKoSXMl3kCTngQeUtkZzTGVkoB6km3zWJxzOElwo1CsMMlwJSTrbhAnShpixC3a--PvxRSxG6UCJDFipC3T4ACklYjzcHFAWbU6Nf37TS2ncA3vvYNWjvXAnkIi_M6gKA9gqfyipA2yxye9l0NLTq-eUYp6xIiQwG4TeV9YwLuzaorrHr9gHsra0Vglsm-WPbr70vN91jm5Ug369_cImLW-p8b0NBq_T-gwat7w_DQXnhncq1bVmYTQhlYdb1mCqZkR59WZiQFTGX7XVZSxMJGVt7v7HrS4nlQxwkywezOFbOUG3PIqs9T8SvWR4YGSRMZVIQJd6CzyjmKNowDjwNrZNPCGPN3-EYRvDIETJ0PLbdHuzDozISQvhBglcBrMsts8ImabkLdI_4yfORwkDnmKYghWUeR0QhvE6wDCKIZXRXw33Pv2XaQKIey1MMpvwaY7DM7Dl5EIb3oLPMIN01wiRswhWatTgBaGtcN3PV0M5Qy633FX8iyCidNvtTzeQetmaj1zZLq9co_xNYA5tRXjQhfHf8Vzg9fn58-PrFq4MNbofOJrf34PuToxOw-bgLsbLENiaEx2AkGMWEzqXGh44V_7SRZpZmJUYM4cg5DhxCxn6Uagf2dMtW8IkHVtDfCGUHTTPIm3se42dSG01ZoM67q-RFQXLk3M2GDJCSy5S_hKD0vNKgDxvt9TWwNJURM1hmuUhJrVdnXZ1QZkGuwHJNWElVj7anqdUDIymmfLYOuPue6LtChJ80Uemb26xU7jcGiQVr66ee6-o2Xiq0zI0NGfOTjuAyfbMjbO55R_je1QipvMqZ1sRSxjJTlmFM5cRDiAojp1NX85zyGcWlY0EbNAImrrhdpV74s5QxPLU0tiMSXDrZhpRzofWcs460022rgIzlF_7ILUMqG-ot8WQUz7ahmCCLlZTZAZXpUmjDhAF8W7A0yOUVKsozGXT-sdfXb5Xp7A27XdCZlCbBGLgwEhgcntn0wWBapCnsDX1QWlegjLbQQOaw7ULKc4iQp1zMqJROERiYRc4jlsLo44ef7tm4Xfs2lUW8wyi--Bk--PZmUBXpxXQ0vNDWN7bm9zpoa-Wp8zbosC585WA6PIPOpLvLgbba3vt7o9tMtruc1fSWzuKqqOaXy-rG7UPg16iz1XdXIXoRzRvT7su_PH1-clhvPe5qx7pzlhYIbwa9vXPfQ623JE3uXJJGhwkYZ0M9JiqxrE_CgsfObyhe5qgMqm6PdgUoKpS8HNvbVN3Fs5Kro2yGV0gZjRkq5W_i4M14VxqIRs_JZGwxPrXPrNblhmfomobRzazE8p1hU-GvTWtV6NaZI3hcMvWb0WDQ-3YwGJzDs3c9wOtcChSGkw1yVBHmpmBpugBKobsCLCrU3Op7vBJTffaNsCR3a4RTFubCtSik71rnYres9BHt1FqO8zvv73zRarX-0Gq1Pt1Dtn75cttYaLhjLOST7tpwyNKxa-6nUkGtAS7HQ6NhN4QXAiGTQrq-G2gY8PLJkStoKEvYqJoqfFuWoOTpUkc8TZmRqlceoZ7GZeaNOUIIP7woYyJBhcvepDkfl9cw0FzM0s2oqqV_6NCYA3KpTZDx664POBa7IUaH1AGnTt9PpLoU3xN2iTFF4zLQ3dNI2l0NfRRzW90rmWooNMZUdGs01kN9ysuZYpkGplDcNfbLwAcHm6RIU4hSIsb-EwrPtBvCclhwYMHtW_i46Lmass8iO56ihrZ98-ECSdraol9Mium0Ul-tder7-02t-rBpwDAcbAbCMgLxOq-KL4cPz97Vhwn03ltPQOoabpmA3HSU4GDeMk1AMQ_9gZr69Zz4200hVsG1JWHrqEa2p-zK-jwN2qDjan8eaRv_9dREnpywHHdRa82f1soX2mzkzCs2RyIyr1i7yRq1V6OYYypzbH4zino5cFohGnp7KQE6KRfIFByedd2sJS5bAF-v76h3rEjrXPfWHDFiC7s6qqzW_LgKkd1shMgzkRX3oDq9cFN0t34j3GzZvjNPo5hfuDCtoUiOYMHxqC1hPDzzJZKoZuhAm0VKSKZFZv8PmCN0rv823OVL11tRojhbS7-_WVFPr55HrPbms8MnBzbTfeXiIK4--lfX8xSXtyzmV2b5_ynoS9C2aeQrm1ar1fri37_zdYsfFFYKl70b_Z9VZtg6G064iBtmslc8nmFFmJtFVixsp0EUP8ullE0wpS9OmmcT_-tNe_WbOP88Vw2brvpMd40aADz7PFft16_6TDd9U79p_pnwu9d01Y67aLRa_-y7cuC67bNKT1j_-mnD5uoRUxaXM4gysv8L
[strum-harp-link]: https://reuben-web-player.pages.dev/#r1.zVnNbhvJEaZvgY8551CYi8iEHA1JrazQgAFbNtaLYLOBZehiKFJzpoZsa6Z70t1DihsYyEPkHfIAeYO8SIA8SVDd88sZShSwAXIj-7e66quqr2r-_evB4K8vAbxYqpSZ2w0qzaXwFjAf0zAX2qg8RWG8BXj292TNVObZ2UiGNPxZ7mAOMoY5DK-n_hxOPq8RPsudPhnD2_efJkEwm8G__jGH0-LvfDZaAINIsdXEyIk9F-hcH34SCEu-gphFqIBrMGsEt2DJFPznb3-nf8hSOLWjp1zAl8D3pzcwpKV3dvQOuDCoYhYiZDxDiBTfcLEqNqWoRsBEBM0dMoMsycN7DQyENAjIwjUYnqJdlknNDZcCQiW1RlqljaIzlzIXEVM7GC6lWUPEFYa0Uju5Rz5c2YUamELQIUsQIlwpRA0KtUw2SJIoma_W9iojBUsglMLggxmDlsAEyAwFxHmSTCLOjBQ8LI5SyCINjAQiFcIq4VozEUn7Qm3YTgMXcI87Hz7Qk2Rsb7mAjeQhalIyg7XUBqNCGdbGEzt9BzpfTjJmwjUMrX4wAs22MHkDGaow15pvEN6-v_oE15dvR6_t2XarAp2n1oCpFYXBUvHV2gjUGr6XMoJ3uTFS0HvRGTrlD5UmGKRMG1QQ88Sg8uFyzcTKGeMed5DQvVtu1nB6jzvCwfDHH97_AGcXvn8ejEAq0u6aZW6LU5Zbv2YqlWJ3qgPfr_-c-3C1RczgtJazAS8jnQ0sGApFkOQwbMkJYW5kHMPvrG0FEyESAtCUSCbI6IyJQhYZGrZBd9HU989ufOdctDtXIWpvAeSi0PQ_ZxtyPmfD0_0p_6uWwnsJ8K3w48Ib6sO4yHJTH14e3xgA8Mwus7fE85mVqhiOMGZ5QjEh8IPGeMpFd4w9eAuY-kEx9K2c84qXP_PGac-N054bz3puvMfdM28773vg2UXPfbSyc2ENpGfr9fy5en3ZuNqTudk3r8xNW4hYSTK3dypz47M84tJrn1XBR8jI2umLHa9OLF_hgFM9mkWRci_2qnBbz5ZJoxXWZebDXRlh72DIYI1JBNcsydHG_CJvTEfdlMCNxiQeQ4wRLHfwSBIY7qeM0WsX5JcUnmxcJ-essoAL0j58Qm00MEMWsEHeyLTw5BFFZyFdwoi5QrsukSzy4aJYo0FuUMEUHOBJyMLQLljeFY5wV0pZh4QRbHmEonhyxoRfK7LjwAdcqmXpYr6a_NZAVKn_g7udnXv3Fi_1FtD2jQ0mMuRmdxiqHTQV8bgfT53JAk6XkLKvUo1hOpt8_vDZhxpfe6m2kWm5tsmVdGszJyXCZFvny8I897irTOMyzRhamUYbzJyJlJQGmEUSUGooLD68nPv-5dmIcohRTOhMahy385A-76arJ6xdvMxbwKuW0kmKg0akGNhvwqATY_TUW8CsPTRzsbU5NPcW8F176KwjlP7OW8Dv20PnBItjceE4RT8s9ucKVHyU5LgXsJ8da06D2of3Vo2l2-_DpDSSozBJIkNmsLRxxXU0DDOZ7LI1ETOLKvL5hGVZI6LY1G_kCs0aHZVlNVvrZ05cgY3MDSSEUsR81UaCk8F5XycJ9UGn9KNDKCnne5FCwe5wjKlCfr23I1JFXzqU5lEIOIbVD4H9uQICP7a4mXT0TZOAEbHN1z10zdL0K74iEu50Z8sHFGV6eddLY59wVpdgDynNmb2dhdtKd3IePCBl2W2xpHd79bpHT1Ct3HCUWzqm0W8T2Zx4vlIKkz5ToHSm-6WpydhtuOkApWHUy2uXP-7qHS4BnGjHSzaWl3wJxtMb0KmUZo0RkQ0JDC6vS-wMmYC7dKbvSs5it03eaDcdSrFBZVCNxjQrIGZC2JrCFYFbCYoKnghSlmlYYiK3VEiIqL9AafGU8zKxlWRGYVF9VfzkccvwwzSgQWp7oZbKyBriymqmyW-pmHZJZn6cKVl2G89nt05j_VZtAH_fpI1y09lq8qZ09y_nQTCeToMguIGPP48BHzIpUBjOkicUE-ZqY4X7UG9pvvEoxREAe3Unc3NbMHsSrpUu7Zw9_jwIjkyax2qQHP8Y9dVB8kvgT8eB_-pmDAkXyNQvAagj9BL4r3p1EvjTTvVy8_LbyxeDweA3g8HgsUJ58M9fHeqDzR7tgxVJq9kNu7YEwzVMYqka3MNeBcOqBeaaXakU0tEGaLZX7MHUJLMBJ1b4F5eEXCSROuRJwgwx3mLJimiJ9fi9rsyQjDOhFzBB3m9YeD8GvZbKQIQh240hkWIFChNk2pVbtiqy4kBRweRm5MMffyoT6RqVW0k3HmjrFPIyoKoqwXY7p5uKYUg9KOqzmUnKH0YVN3eUjWv4mmtDTycgothQv8mva7yF1dKp1QNRdJtbTlloSBNU4nrHt0LopIMV--0yj-MG1WgV7mdnfR2DWV_tPgv2PbyKLfiQNY_PBbfA-_hzVaFXvkbv_d83bZpBpq-54NR8oL-AYuMXCzridxnAL9eXqL3kAD3RYSfoXbHt43GsA40WY6HJ3gC2ZRuk4FLecVT0RkH1c4b94qPospg_1c5v3XcBT3u_kKBzbRgXB32fvJUbDXIrfPiembrV4ojr60ZPRmYnGjLGFUa2LTKhfBvbErza8BRXtmI6VAazFoZDtuugteMCTZPYyQOU2AY8e9xFsyx1ynhGyyKTW1RP5lkUm1vn4R3QVWW_S6VQGp7YpOOVoklRJtrsElJlkqcI9kwYPvx5NnpCrw8HtUQu2sq9x7GLPDnq1ZuQdd58ffl2YTPdb8F2PaLmo5_Cx-EGlw77jb187OmFWf5_Kp9SaYckKrjNYDAYvDh5UXAXndu81iAv0yM-4tFnJiWTRhgFb8lF1GjsgpewJSYWqeXIlkcrtAfaL3VepaV6d9lsbO7_RBVN32JqSzUX_gF3fcsatUdz9bvWcFe2Ql__BQ
[euclidean-drums-link]: https://reuben-web-player.pages.dev/#r1.zVzhctvGEU7_ui-xwz-hIpIiJdlxlDozrq2M47qtJ_Z4ppOm0gk4EmcBd-jdgZLSyaP1Qfo2nd0DSEAEQAAELfmXBYC3e7vfLu7bvcN_33_11X-eAAzmSkfMXiy5NkLJwRmcjPCykMbqJOLSDs5gwBMvFD5ncuzrJDIDesRXHt57CYaH83EcsjshF3A69gImJQ_hPPsR6ODOBhFEzAuE5DC8Ft71CIxkmo_AqmgEAbMHE_i75GACprkPXqi8axj6YilQrxenMP4BGMye2WAsleWw0MI_AF-LJTcwV4mGS6flJSy45JpZpc33wJkXgIk1Z74BYQ1cxklouLmEAP_iSy7DO2CeVsbApbE8NpcwvBE2gEutLLNCyUuwCkwg5hZswCEOmOEHwKQPPMJBGCyY5WADZmEuNDfuv6kdvjaARoOlEh6fwPmS6zv3BwgD5k7agBvxG_dhrlUEKk51h6FUYFgUh9wcjEiED8ZqJhaBBTWfkzJuzk6BUFxzuhhxq5VUEYfxGNDY8AIMWv5K-XdwCD73nLNiYb1g7GsVp-6AFyCVMBwOwSrJQtBCLshH8AIYXCkVCa4hEIuAa3B-xLm-oEsxM4b76QhWeNcTeM91Zgi4UwksuD3LIcNT0moVGhiS6Y-cc44yyx-MgMHr81cv_wHXUl3BEGfHohi4XPJQxRyNi5PBiSpJCh2hssKiqbj1Am7gStmA7GKZCMlv5EacO5gbzmMScyViFTINr9_Cjz-9-3j-cypyPINQ3eDUwFc3EiYTmKKX6H-Hs9XEIaFxpA8M3p1_On9Hv5_Ax4A7fKZmMGCSCGygVbIIgEHEjOUarBYR4gxVU4mNE5thhdQQBpgEIS3Xc4bQkXFiIRYxh-HL1z-Pp9PTk-8h1txw6WwHIYWGkGAS-o05uhfGk89GyYMz0MLnYHkUK1gKBkf03yMhvwfNTcBiDjGzlmtp3P0_4Ux-uEh9JeRodck5MX8l8yQNR8ZGYPHCQHMRWq6PhIRfxrPJZPbr92C4hVAlvuSmKDTkSx7So9P1k-TX_FOEiSMhnfVNpJQNMMCcIBoDXn0yZCLEDRqdbIuQL6p14S3JqQUF8KJUPjc59-ZFk2MImiEGlc_nLAktTCfT6Qw0sxg8NmBOsOZWYMoTMsQYDYXlmoX48BndDxXzMYoU1x4feyGLYu67FJM9m-ImFxWayQWHeaiUHoFRqeifPjh8hT7w-Zx7FuevubGwZGHCYcgcpDKFVWINggOzXDoZPx0aAUmqAdda6QP4Z3I8nZ3ClbBj4XNphb2jh4zh2tL0IHvXnFxofqOF5RNtJoP0fZMie3AG-F6iS3FizepvgAEBM3cBL93F-JPB_OSYBkovpxMYnMHseDqZ5u5EAl9ys-I1djs4g----65wNZGCXn5_fv_XQXr19-z2ADNNGgEtFTotUWdaos7sWak2qcxyhSj-2hroWUP7VCjkZJbrk0V_S5XKPFZqoqetNaLwbK_OdFamUPEqqVShUYU2LsX0YJ1xqcMm03KxlMNaS33-tJlTSqTS2qJbuBzvJ1ycRo8oXpxCHQOmcU5pFTBOpUcTMU6dLx8yTm63mHnWOWSsiroFzMl-Agb1eUThgup0DJbGGaVVsKBCjyZUUJkvHygo9UuHScBstzB5vp8wQX0eUZigOo9qEYYKPZowQWW-fJig1G5h0noB9iQneeCqCgVCpRJbVAJLUKjEkUrshCW-UIPiWE_S8QZEfwdn8AtdX1O0dBZUv1vNY8B8X3NjaOh7t9Iy4gdX97MiEnIxgWLhD0krlbhc2MHpLcRcwxVnFjnr_aqgK7-hGKrw6bu0TDZJqxxY8DA0qKt1EOmdrFXaYJ6oZqrQxmprk5Tm7ejurm79XuaZDeM5bcutRwt4nmzY7y9Y5lsX1lY10AmcD2fPRqej6QG8oErFWMmxDfiYigNE-dGoqaFOgQL3YIs1nA-rJk13J-iw3MxzJitJ2Pmf57l16e9LM9rGCKunSsfYTNIbAxRSWGPfpTWYOu_JZbn7WBQfurJkNkpW9726y1d7yTYTuKREepmH85rjprVBvOoKqGmp9eAMQiUXXMOLVTmX6mcv__Y6u-N0wLLwFhgwaxnh4H4i3kzxG9Z1j5T6hpBT-2Oe1MBL85Azw51az_M-T4xl2Zu1kTejY1PjSKpnXrDIbrjzlZLGMmnTCrsrfLJIJdLC8M1v28KLdDzGylUzLZPwYn5yfGHEQrKwRuG4DHof8sp9s4IevPqUpd5iv6ARMLZE1spwpQ682uJ8uZx4y7aBWe_KKwRMpReph4KPwFzzfydceneN3Xj6vKEXme838yLqsKErqYh3MK4ZdXBWbiNz7-Yxsk8XZxHkWrpKGU-EIb69aqygjLcJ5azJNco5inqEMkuh65bXFoOQlWunRk-U2uSGLTkWuzOdBo2mHasbrptBwEv0chOv57exklh6Z-HY2LuQw1KFScSBHofh7b-OD7J2B3bRXn3aYoPbvQRi02y19NjGFD-9enmWhiP2Mr8BalD5zaazBeOIqC4Qd87oNR2tek-bEKdmVv51755N3_ev346zv5URuPwZZV21A2xxKWDwm4hjrsdzzfl2m4ktq6yUP5XaLVI-X-ucZzVWRO79PGtkLP9zKqbaYv7nDVO953q1GeH127T7l-H_mtrTmZHuJYlVx5BauFtARSyp1kYI5PKFcCq-iYkvCpFWWO4YJZn0nEGPn_YAv6zJ2QB9rpnqwEdt71Haml3BbWeIObLcFWHT436zEovi8pcvAQG-gay9vFsy8j93ykUrz7VMR3WU0xXASzjnB9oyUk06j0enSDoRLFfMu3Z0nVgVPKWm-uzkQYlmoS1VHlvbmOa9tk3pIPVUM9-Jauu0Wq6Zuq1sxU9-y--jacszc62hKqJJO4piCLlc2GCbmzsSyXyHqtT0tUwyw3VTKpmvUrenks1Wd06nB1_erbDTYX1H27_qZnfvgQIo3d4xXNtNBjvn65y46sWkk9jXQjI_w5b5O-_6vhaTbkzccXUxr6a3bqtfkdriZh23FsIhcEcR1mUbvMhnz582jIhtNM8pX8bzPpLCnopcQMAQdzceQCJ97pQmB-xC8AqGK3VkN47XHLYkuxK1tImuX9B2oD87QLZJtcMNH4nbDRsUN6um21rXO1b7CN_qVfs2e6w8128UN-SE-YcfgBQWdmmUmu8LscJ0bdCSFtKvdmJ8a9CWL0m3Ub77zq5Yl_TJ-XI7XBrCa7-sL7_jpiuEeqR9Tp0y3ufSUG_EbwXZTmlnH9QPt3SUEL-PKqqhfSejY0f7MJfQMQt3ACM7ZuH4IO5LxjMVuC9baWAYiCHMOQ8fkhLmtl2VB942QljYmFQ6RD0dXO-zauvGWjJIjiyhgujJ3duOq81Pj7nruN6hVeqWWqro4qAhUZwd70QUK9M0QaNzyxGPyCy4tGH-7AFVQZss6nvtRVKQdWtFWhVlZ356akgWrdoy86Zh1Wc7Eoes70biAaeSZiQ6OD2Y1d6_J03922TdTtsPy5qUqPleepQro3Xw3z46lDjugzYoVy4oNcg--5Mo-cHrVzvEZcP8VcPMo357kxmWOkC732LSelfxVpawfvQBKGhu93Opzb4QAaX1Rkv6aVW0E_nMkFm-9N1GPYsOrljn9Ek8VzvGGwFqv6RzvXu9K2h6pJyoTBnhxOTSG91MAdohr-yDauK26BKq-YbZGqr53G1rzR-Lpy7jcwTO0G1pVXTcFjnVgzYbcycVyiNrG7Ms7OUvHaKeWa6PJrT0Wk1DB8csb-eg31x11qhEe3xLO6cukaKQYDMW3mQH7-n0s5Drwn-WDiJuWRgKDz9RsFNaXU-z1O5eYtV8PjiDb6fTezvy06zxJh5UJtFm75p6ek_BU0KoPmEAmEBpS59p2KXluzq5UcPxnST88gN-HIE-ktDA9N3o_PokSalPaum8yzVN-74nO9H5Zqtm1Gj3VTP6eIdVc4qiPa2acfTKVXOwShd9LZvTrNHy7bb2Q1-r5vUho62LnPWjD7Bqzh2GeshVM4V1y1VzH-m9-6q56OAvsGpeHSBrBKj9rprXh9kewaoZlSlbNdOrr69VcwrQDnml66q5ScktErcXm6mV9iUeNmtq1u8rRLO2nPK6bban-V6V0qPDJumglt10mGuGvP3MdGOe9AW1QzLwARzC0KroED-hhlXyeRKGcC0sROK2sxkcnFoawfmkr_cmfhCrLMGtyuD5D2cNA858rVSE3zpSS65DFsdYVqb2o6_k13ggVGxlgOmh2m97S0mo3cYUUs2zlEQzcC2ObBq7OK41oU8N3dJx7jhx-axV_kaHKhYarUqdJwC_Pvn9yVfpvz_874_plwzTz5zlPmVIZKLJpwyzz9Btnme-EtKnzELneFdzCtkVnd8efCxevxH-gpMczXzBwnLSmw2aP-i6OTQdCX1_7-6GgNWdhVZJnI3aQK4rBVSI_VC82Z_UVQGjQvDPG_f7k-3YYoXg18Wb_Um9v9i9J_Yt_Hjvgf5Eu4VZheR3xZu9SC1sqN-U67b4doA0jdtEdBWoneT2qG4suAbXTnYnYDcWXwVtJ7s9thsLrkR3KrkTvBtLrwK4E94e4Q0E5zYIlbwNVNQF3lZF24VWQRtltgd2I5E1oEapnSDdSHAVnFFqezA3ElkJZJLZCcaN5FZBGMW2B_BWkbkuxKZMrNl3gG_A7HahVfBFme3h20hkDXxRaif4NhJcBV-U2h6-jURWwpdkdoJvI7lV8EWx7eG7Fpmu8v8P
[mic-space-link]: https://reuben-web-player.pages.dev/#r1.pVfLjuNEFA0bkHrJFxx5M4kmcdwPNMi94a1ZIEDMiA2NxhXXdVwzdpVVVU7SPWqJj2DJnv_gU_gSqCrbcTppMQxZJHK97q1z7jnX-eyjyeTtGRAVStfMvtqQNkLJKMXl3A0Laaxua5I2ShHVIl-YhuUU-Umucjf6rdjQQsimteBUK0w__-rHRZJcfjpLwWBVs6hoQxXCkkY0hJVqJYdVqNRa5Kyfy0smJVVIUBBxA1sSJBlLHJmPm6FhNi8x3edlln6IzNKviF8bJWf469ffoCShEBXBlGor5BorZUuUrNqQgSr86T6bWnGqYmS1yDMIAyYhpCVdsJxGWacQFrWQ1mBZixxChgQVJ0hWk48PJvkcK8pZawi2FAZrzZrSndtU7JY4mHWgwIMyh7AGWXfxFEmGlZDcZauJcfMoQNNb1WpwKlhbWfTob0ROTwwKoc2w-BqaatZgK2yJzOWAxUKoRc2abA6ucrMMGxeNVg6vuOazGC9LgmlXAe9cSdPWZBwCzAOkNBeS6VtshSZMs7c3UaFVfROluIkcPDfRfRZ4ENIITgOZc-yJemKgthKZkFmgQhgUxGFLrdp16feYW2lLMuKOOBwjc0jakMbqFgw1W4scJdN8yzQ5zFx19AWIP_-47K6y0orxnBkL1dqhDqc98hlULawlPoMh2ZXeliyMWEtWuVL11WMsaVI9tCbGDw5PYVNkmtoVSU8yxuU5iMbfOAs8MHl7QBoaTYakxdQXX78btmTWV4Q5rACDggn_bSy2JUlIdXgg7YSxxhMwguPZzBUoTEPszbJL1Sp_21rk19h6ut2jU-CK5W9CncZ4UbKGUIkNQTn4v3_xZYqguaVVkpZCYpq3VhXFHM_vZvN-Mny7We14W6EWuzmSOD6fxXDe0eXthAfDZG6FksQhleRkSddCCmNFHkzDldyY3_OkE3tRVEK6CpCcNIR8Tbk1eCNdfa3aoiBtPLDEeBzsS5NRrc7JRCmcBwJRMLcU0bGnRGfAfeeJnTfs9_kb7M-Bt8rRIxDZ28afXFxevAr5-Cy62Y7WKEXSDd773_uwJgpFexBBtfYwgtOfi9Al7eYPzxpu4CzLnfWzHx9O7FPsZT8kGDHONRmzP30_1TWBl3urdtXwtOOaioJyex1czql8IHEsd5e5L7p9s3AG65bE-0hHIPuxg-cxCo6BYeJ-hHVPchJ_Mhp1Wbu2lyRJPHAwxA54pP3mAc9fzu7PPphMJh9PJpPjmpn8_uF_aa_HrfU7MpatKvKY4mlwTu9QHbKDFq6WQ8et1LZhxgRlM3yjyXMxB-0aZUbuum9yDmkD45aXytiuy7ZmcHwX3lVNjC86AtPA55S1XKjZHJlL0T3DbKmxCFbgT3azXfueOpZDbSxqscNPrGpD-BmEvEamWtsfOj-sjbBr5vw7xlfCePeq2Ioqs9wKviZrgj0J6YykDTfjKp9DKgslu1t2iF25zvC1axej9xJOecW0v3XoTE4S3jFJroWkBclC6Zw4NJNrGjnRTZsk7NnFDDftRXJ-1cPxhqgJzaQQlSXdweJ6dFtVuEji-CJJkgTP78KRc9QOe_cCsHXe77Zuzvey6RO47nuyNqEFe5yEPWrFLg_fizvXeyf3eiCrU-Z1JJFeVqe3jc2ue2vxErwca7D2cZM4GY-xXZTi_IQmO8W-s8fuw155kR8FvjgV2dNzMJ63euPD0K4Zn99K4WX8_O6B776Ph7taj70M3tPEQ72dtvCHc72HO5XnSlqtqhRrkraiwU1YSYz3r81Bi__izSH7x-xZyNPuHATy6DZP-n7jKaCPoAjZnobicKYDordMWCaqa_fHYOf_GLir90p8YgZT85f_X2AEQg75PoSlFrtHd4960sNtWvk1SfzsqIpc65r88_kb
<!-- END share-links -->
