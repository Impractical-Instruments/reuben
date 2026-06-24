# reuben — Roadmap

Living priority buckets. Maintained during design sessions. An item moves down a tier only by an explicit decision, not by drift.

## Tiers

- **Now (MVP / spike)** — the smallest end-to-end slice that proves the engine: a graph that makes sound. Throwaway-friendly.
- **v1 (first real release)** — the actual product people use. Linux + Windows, Toys, good-button UX.
- **Later (post-v1)** — real intent, not yet scheduled.
- **Someday / Maybe** — exploratory; may never happen. Parked so it stops competing for attention.
- **Non-goals** — explicitly will not do. Recorded to prevent re-litigation.

---

## Now (MVP / spike) — headless "it makes a sound" spine — ✅ DONE

Goal: exercise the whole spine end-to-end as cheaply as possible, then get out of the way for UX/Toy work. Driven from any OSC source (TouchOSC / Max / scripts) — no GUI. **All items shipped.**

- ✅ Rust workspace: portable `core` crate + `native` crate (audio out via `cpal`) — the ADR-0012 seam from line one
- ✅ Signal + Message types; topological **Plan**; Instantiate → Render loop; single-Lane fan-out (ADR-0010), expanded to polyphony (Voicer + per-Voice replication, steal-oldest)
- ✅ Determinism invariant baked in from the start (ADR-0001)
- ✅ Executor *interface* present, **single-threaded implementation** (parallel pool deferred to v1)
- ✅ 5 operators: oscillator, envelope, filter, Voicer, output sink
- ✅ **OSC-in boundary adapter (UDP)** — `/voicer/note [midi, gate]` from any OSC source (ADR-0007)
- ✅ JSON Instrument format + type registry + auto-generated JSON Schema; load → Instantiate (Swap-from-empty) → Render → live audio out
- ✅ Default 12-TET Tuning; symbolic-pitch seam present (ADR-0008)

## v1 (first real release)

The actual product. The MVP proved the engine spine; v1 is **the path to a delightful playable Toy a non-technical person enjoys**, then the reach to ship it on two platforms. Sequenced into phases by dependency, not by tier. The through-line: front-load what Toys and the good-button surface depend on; defer engine-internal performance work (parallel executor, hot-swap) until it actually bites.

Each phase lists the open-design threads it forces (see [OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md)) — those get a grilling session before the phase starts.

**Status:** V1.0–V1.4 and V1.6 all ✅ shipped. **V1.5 (the finish line) is the sole remaining v1 phase** — grilled and rescoped to three items ([ADR-0026](docs/adr/0026-v1-finish-line-osc-out-and-stereo.md)): **OSC-out**, **stereo output + a `pan` op**, and the **Windows build + a release workflow**. The other former-V1.5 items (live hot-swap, MIDI, clock sync, full n-channel, the parallel executor) moved to [Later](#later-post-v1) by explicit decision. reuben is an engine *always driven by something else*, so a dedicated UI is out of scope for the project entirely.

### V1.0 — Engine hardening (only what the rest of v1 leans on) — ✅ DONE

- ✅ **RT-safe Render** — edge-buffer arena + all per-block scratch preallocated and reused; zero-copy events. `render_block` is allocation-free after warmup (asserted by `tests/rt_safe.rs`).
- **External OSC timing is block-quantized by design** — *not* a task. Reconstructing a sub-block `frame` from a UDP datagram's arrival time is fake precision: arrival jitter already dwarfs sample resolution. Sample-accuracy is an internal property delivered by the Clock; external messages apply at the next block boundary (see `crates/reuben-native/src/osc.rs`).
- ✅ **Clock + musical time** (ADR-0006), first slice — the home of sample-accurate timing: a `clock` operator with a sample-accurate beat phasor at `tempo`, a beat gate (sample-accurate trigger), and a `reset` event (sample-accurate locate); phase in f64 so the grid never drifts. `instruments/metronome.json` clicks on the beat. *Remaining for later phases:* meter/bars, musical-time timetag resolution (schedule "beat 2.5"), and groove/swing as separate re-timing operators.

### V1.1 — Operators for music — ✅ DONE

- ✅ **More operators** — Toys are assembled from these. *(Operator-authoring contract is now concrete — see [docs/agents/authoring.md](docs/agents/authoring.md); the rest is mostly mechanical, parallelizable.)*
  - ✅ **delay** — feedback echo (`/delay/{time,feedback,mix}`, `instruments/echo.json`).
  - ✅ **reverb** — mono Freeverb (`/reverb/{room,damp,mix}`, `instruments/reverb.json`).
  - ✅ **LFO / mod source** — sine modulation on the sample timeline (`/lfo/{rate,depth,center}`, `instruments/vibrato.json`).
  - ✅ **Sequencer** — clock-driven step sequencer; walks an 8-step pitch pattern, one note per beat, and **emits `note` Messages** into a Voicer (`instruments/sequence.json`). Forced the **internal message graph** — operators emitting Messages ([ADR-0014](docs/adr/0014-internal-message-graph.md)), the foundation note ops, the tonal-context bus, and meta-effects all build on. So a sequence is polyphony-, transpose-, and snap-composable, not a Signal-domain dead end.
  - ✅ **Sample player** — one-shot trigger sampler downstream of the Voicer; a gate edge fires a sample, pitch shifts the playback rate (`/sample/{root,gain,start,channel}`, `instruments/sampler.json`). Forced the **resource store** ([ADR-0016](docs/adr/0016-sample-player-and-resource-store.md)): a top-level `resources` table, a central `Arc<ResourceStore>` bound through a two-phase `bind_resources` hook, a `ResourceResolver` seam (core owns types; native owns filesystem + WAV decode), and degrade-to-silence-with-a-warning on a missing/bad sample. The bank-ready read seam is in place; streaming, loop/sustain, and multisample are deferred.
- ✅ **Tonal-context / harmony bus** (ADR-0008, [ADR-0013](docs/adr/0013-tonal-context-bus-mechanics.md), [ADR-0015](docs/adr/0015-latched-context-read.md)) — a `context` operator broadcasts the latched key/scale/chord; the engine carries it as a `Copy` struct over a **context arena + third route lane** (`io.context`/`io.publish_context`), re-slicing followers sample-accurately. The Voicer resolves `degree` notes to Hz through it (so a held line **re-spells live** on a key/scale change), the sequencer emits degrees, and a `snap` operator quantizes arbitrary pitch to the nearest in-scale degree ("always in key"). Rigs: `instruments/scale-demo.json` (re-spelling sequence), `instruments/autotune.json` (snap). *(Deferred to later threads: non-12-TET / Scala tunings — the registry seam is in place; a sequenced chord-progression op — the chord field + resolver exist, driven by Messages today; symbolic scale-name args + cross-scope context layering.)*

### V1.2 — Playable surface — ✅ DONE

*Grilled → [ADR-0017](docs/adr/0017-playable-surface-and-control-domain.md). Both items below collapsed into one insight: the build is **new Operators, not new instrument-format machinery**. Control is **Message-first**, Signal/CV is the opt-in special case, and the two carriers convert only through explicit operators. A **Good Button** (the official term — principle and artifact) is composed from `map` Operators + existing Message fan-out, so it needs no format change. The Instrument-boundary-as-declaration (surface → `Descriptor`, stable node `id`, rename tool) is deferred to the nesting/contract thread.*

- ✅ **Math-operator family** — one `Number`-generic core + a `signal_pointwise!` macro (`operators/math.rs`): Signal **`add`**/**`mul`** (base-plus-modulation; identity unwired defaults), Message **`map`** (1:1 affine remap, ranges + curve — the Good Button workhorse), and Message **`differentiate`**/**`integrate`** with real frame-based `dt`. *Binary Message arithmetic (per numeric type) is deferred to **port-tagged Message routing** — a delivered event carries only its address, not its destination port, so multi-input Message ops can't disambiguate operands yet; combining two streams lives in the Signal domain today (ADR-0017).*
- ✅ **Message→Signal converter** (`operators/m2s.rs`) — the one sanctioned M→S bridge, a single operator with a `mode` param (snap / slew / smooth / glide) + `rate`/`time`/`default`. (Signal→Message — envelope-follower machinery — deferred.)
- ✅ **One-port-one-type operator sweep** — oscillator `freq` was already Signal-only (param = unwired default); **filter `cutoff`/`resonance` → Signal inputs** (params survive as unwired defaults; constant-control fast path keeps existing rigs bit-identical). Standing authoring rule in [docs/agents/authoring.md](docs/agents/authoring.md); cross-domain wiring is a `PortKindMismatch` load error.
- ✅ **Curated control surface (Good Buttons)** — composed from the above, no new JSON section. Worked examples: `instruments/good-button.json` (one brightness knob → cutoff + resonance) and `instruments/auto-filter.json` (base + LFO via Signal `add`); human OSC walkthrough in [docs/v1.2-playable-surface-testing.md](docs/v1.2-playable-surface-testing.md). The first-class *boundary declaration* (public addresses, stable `id`, refactor-safe contract) waits on the nesting/contract thread.

### V1.3 — The Toys — ✅ DONE

*Grilled → [ADR-0022](docs/adr/0022-the-toys.md). The payoff: **instant music for a non-technical person**. Depth over breadth — **three** Toys, one per distinct player gesture (rhythm/auto, tap-harmony, continuous-drag), not the full archetype list. Melody-player overlaps tap-harmony and meta-effects overlap the existing fx instruments (`echo`/`reverb`/`djfilter`), so both are deferred. Each Toy is **one self-contained Instrument JSON** (the unit `control-surface` consumes — V1.4 shipped ahead of this) + a generated `.tosc`; internally a graph of existing + a few new Operators. The build is **new Operators, not new format machinery** (ADR-0017). The generator draws only fader/stepper/button widgets, so every gesture reduces to those three. **All three Toys shipped** (PRs #37/#36/#35), each with a generated surface in `control-surfaces/` and a hands-on testing doc.*

- ✅ **Groove box** (`instruments/groovebox.json`, `control-surfaces/groovebox.tosc`, [doc](docs/v1.3-groovebox-testing.md)) — free-running multi-track **synthesized** drum machine (3 lanes: kick/snare/hat; no drum samples exist, so drums are built from Operators — the reuben thesis). Each lane `sequencer` → `voicer`(1) → drum-synth subgraph → lane volume → mix → master filter Good Button. Kick = osc + pitch-drop env; snare = noise + tone, env; hat = noise → highpass → short env. *Surface:* 48 step toggles + tempo + 3 lane volumes + master filter knob.
- ✅ **Chord player** (`instruments/chord-player.json`, `control-surfaces/chord-player.tosc`, [doc](docs/v1.3-chord-player-testing.md)) — tap-to-play diatonic harmony: 7 buttons (I–vii°) → new `chord` op (stacked thirds via the context bus, always in key) → poly `voicer` → pad voice. A key selector (`context` op) **re-spells chords live** on a key change. *Surface:* 7 chord buttons + brightness + key selector.
- ✅ **Strum (harp)** (`instruments/strum-harp.json`, `control-surfaces/strum-harp.tosc`, [doc](docs/v1.3-strum-harp-testing.md)) — drag-to-strum: one big fader streams position → new `strum` op emits a note per string-crossing (scale degrees via context) → poly `voicer` → plucked voice. Open-scale harp glissando; no new widget type (reuses the fader). *Surface:* strum fader + brightness + key selector + octave-range knob.
- ✅ **Engine work it forced** (all backwards-compatible; new ops via `create-operator`, test-first): `sequencer` `gate_mode` + per-lane `pitch` + 8→16 steps; **new `noise` op**; `filter` `mode` (LP/HP/BP); `clock` `division` (gate N×/beat — a thin slice of ADR-0006's deferred subdivision); **new `chord` op** (degree-in-arg, sidesteps deferred port-routing, future-proofs the sequenced chord-progression op); **new `strum` op**; `control-surface` generator emits custom `[degree,gate]` button payloads.
- *Deferred (not V1.3):* 7th-chord toggle, clap/toms, chord-locked strum, multi-touch/XY widgets, single-op `drum-sequencer`, per-step drum pitch.

### V1.4 — Good-button UX layer — ✅ DONE

The surface a human actually touches, driven over OSC first — proving the control-surface API before any native GUI commitment. The **TouchOSC layout *is* that surface**: a player holds an instrument on a phone/tablet and OSC goes straight to reuben's node addresses, no engine I/O work and no GUI commitment. (TouchOSC *was* the roadmap's "TouchOSC / web stand-in" — the web stand-in was an *alternative* carrier, not an additional requirement.)

- ✅ **`control-surface` skill** ([ADR-0018](docs/adr/0018-control-surface-generation.md)) — generates a Hexler TouchOSC layout (`.tosc`) from an instrument's `control` blocks (Good Buttons, params, a `note-toggle` play control); on an un-annotated instrument it infers candidates and **writes `control` blocks back into the JSON**. Adds an opt-in, engine-ignored `control` block to the Instrument format and a resting `default` to the `map` operator.
- ✅ **Hands-on proof** — `instruments/good-button.json` and `instruments/djfilter-demo.json` ship annotated, with generated surfaces in `control-surfaces/`; human TouchOSC walkthrough in [docs/v1.4-control-surface-testing.md](docs/v1.4-control-surface-testing.md).
- *Deferred → [Later](#later-post-v1):* the reactive **auto-UI / native-or-web app** (an app that builds a playable surface live from an instrument, plus two-way OSC feedback). This disposable generator de-risks it by getting us playing first (ADR-0018).

### V1.5 — The finish line (ship)

*Grilled → [ADR-0026](docs/adr/0026-v1-finish-line-osc-out-and-stereo.md). The old "Reach & robustness" bucket conflated "things the engine could grow" with "things v1 must ship." The cut: v1's stated definition (Linux+Windows, Toys, good-button UX) is met except platform reach, so v1 = **three items**. Two framings drove it: reuben is an **engine always driven by something else** (so a dedicated UI is out of scope for the project, not just deferred — it belongs to a consuming app), and the only in-scope integration model is **out-of-process** (OSC) plus **in-process Rust** (already works via the cargo workspace — a non-issue, settled when the first consuming app is built). Build order: stereo first (the riskier core-contract edit), then OSC-out, then Windows + release.*

- ✅ **Stereo output + `pan` op** — mono-only output is indefensible for a modern audio engine. The Signal stays **mono (one channel per edge)**; "stereo" is two edges on an **N-wide logical master bus**: a tap gains an optional **`channel: <int>`** index (omitted → broadcast to all channels, so existing instruments are bit-identical), `render_block_multi` fills N buffers (mono `render_block` kept as a channel-0 convenience), and **audio.rs owns the logical→device map** (the one home for non-stereo-device policy). New `pan` op (1 Signal in → `left`/`right`, equal-power −3 dB center; pan amount as a Signal input per the one-port-one-type rule). Example `instruments/stereo-autopan.json` + [hands-on doc](docs/v1.5-stereo-testing.md). Full n-channel and a multi-channel Signal stay deferred — stereo-as-two-edges sidesteps them.
- ✅ **External OSC I/O (out)** — the lingua franca crossing the process boundary outward, as a **boundary sink operator** (`osc_out`): core collects the sink's input Messages on an **outbound route** (the fourth lane, mirroring the context lane's publish mechanics), `render_block_multi` gains an append-only outbound out-parameter, and native gains `osc::encode` (the inverse of `decode`) + a UDP **sender thread** (UDP I/O off the audio thread, mirroring OSC-in) to a **static `--osc-out host:port` target**. The **node's address is the outbound OSC address** (one sink = one address; the engine stamps it on drain); no target → drained-and-dropped, warning once. **Message-domain only** — sending live Signal values out needs the deferred Signal→Message sampler. Unblocks two-way Good Button feedback (and, post-v1, the reactive UI).
- **Linux + Windows builds + a release workflow** — the stated reach promise, packaged. CI matrix `{ubuntu, windows}` builds + runs the non-audio tests; **Windows audio is verified by a manual smoke pass on real hardware** (CI can't see WASAPI device init). A **`v*`-tagged release workflow** builds `--release` on both platforms and attaches a bare binary in a versioned archive (zip/Win, tar.gz/Linux) — **no installer** (reuben is a headless CLI; the crate is the primary product, the binary a convenience). Build-from-source stays the documented primary path.

*Deferred → [Later](#later-post-v1) (each by explicit decision, not drift): live hot-swap, MIDI I/O, clock sync (Link/MIDI/OSC), **full** n-channel I/O, the parallel executor (still perf-gated, [ADR-0019](docs/adr/0019-performance-benchmarking.md)), the in-process non-Rust / C ABI boundary, and the dedicated UI (out of project entirely).*

### V1.6 — Agent skills

- ✅ **`create-operator` skill** ([ADR-0021](docs/adr/0021-scaffold-operator-and-create-operator-skill.md)) — authors a new Operator (Rust + descriptor + tests) end-to-end: grills the contract, scaffolds via the new **`reuben scaffold-operator`** subcommand (skeleton + a sorted `mod.rs` insert + an intentionally-red test; the operator self-registers at compile time, [ADR-0024](docs/adr/0024-compile-time-operator-registration.md)), implements `process` test-first, and closes a build → `gen_schema` → `clippy` → `describe` → `validate` gate. Retires the "Developer skill" label.
- ✅ **Patcher skill** ([ADR-0020](docs/adr/0020-introspection-and-patcher-skill.md)) — the `patcher` skill drafts/edits the instrument graph and proves it on the real engine load path. Settled the **introspection/query API shape**: a thin CLI over the live registry + loader — `reuben describe [op]` (operator ports/params) and `reuben validate <path>` (load + plan, no audio; catches kind-mismatches and cycles), both with `--json`. The `reuben` binary moved to clap subcommands (`play`/`describe`/`validate`). Live-graph query (inspect a *running* rig) deferred — no consumer yet.

## Later (post-v1)

**Former-V1.5 engine work** — moved here by explicit decision ([ADR-0026](docs/adr/0026-v1-finish-line-osc-out-and-stereo.md)), not drift:

- **Lock-free live graph hot-swap + per-operator state preservation** — edit an Instrument without dropping audio (live authoring). Swap-from-empty already works; this is the in-place case.
- **MIDI I/O** — drive external gear/synths (boundary adapter; core stays OSC-only, ADR-0007).
- **Ableton Link / MIDI clock / OSC tempo sync** — feed the Clock (boundary adapters).
- **Full n-channel input and output**, with easy defaults — the general case beyond V1.5's stereo (which keeps the Signal mono and does channels as separate edges; full n-channel is a multi-channel Signal touching every operator).
- **Parallel executor** (lock-free worker pool) behind the existing trait — built when the serial executor stops keeping up, not before. *Measurement gate already in place:* two-layer benchmarks (`benches/`, criterion + iai) + a deterministic CI compare-against-base ([ADR-0019](docs/adr/0019-performance-benchmarking.md)), so "stops keeping up" is observed, not guessed.

**Other:**

- **Reactive auto-UI / native-or-web UX layer** — an app that builds a playable surface *live* from an instrument, plus two-way OSC feedback. **Belongs to a consuming application, not reuben itself** (ADR-0026): reuben is an engine *always driven by something else*, so a dedicated UI is out of scope for the project entirely — this lives in its own repo, on top of reuben's OSC contract (V1.5's OSC-out sender unblocks the two-way feedback). V1.4's TouchOSC generator already gives players a real surface.
- CLAP plugin hosting (in scope, designed-for but not built for MVP)
- C ABI boundary for embedding (cbindgen)
- Embedded executor backends (game-engine job systems)
- Mobile + wasm targets
- LV2 plugin hosting (Linux-native)
- Piping music data to non-audio targets (lights, video) — plumbing exists via OSC; actual integrations here
- Agent skills — end user: natural language → Toy/Instrument/Rig for non-technical musicians

## Someday / Maybe

- reuben packaged *as* a plugin (CLAP/VST3 export via nih-plug)
- Other realtime-host embeddings beyond game engines

## Non-goals

- VST3 / AU plugin *hosting* (COM/Obj-C moat; not worth it — CLAP + LV2 cover the need)
