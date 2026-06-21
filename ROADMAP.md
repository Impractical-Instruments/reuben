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

### V1.0 — Engine hardening (only what the rest of v1 leans on) — ✅ DONE

- ✅ **RT-safe Render** — edge-buffer arena + all per-block scratch preallocated and reused; zero-copy events. `render_block` is allocation-free after warmup (asserted by `tests/rt_safe.rs`).
- **External OSC timing is block-quantized by design** — *not* a task. Reconstructing a sub-block `frame` from a UDP datagram's arrival time is fake precision: arrival jitter already dwarfs sample resolution. Sample-accuracy is an internal property delivered by the Clock; external messages apply at the next block boundary (see `crates/reuben-native/src/osc.rs`).
- ✅ **Clock + musical time** (ADR-0006), first slice — the home of sample-accurate timing: a `clock` operator with a sample-accurate beat phasor at `tempo`, a beat gate (sample-accurate trigger), and a `reset` event (sample-accurate locate); phase in f64 so the grid never drifts. `instruments/metronome.json` clicks on the beat. *Remaining for later phases:* meter/bars, musical-time timetag resolution (schedule "beat 2.5"), and groove/swing as separate re-timing operators.

### V1.1 — Operators for music

- **More operators** — sequencer (drives the Clock's beat grid), sample player, LFO/mod source, delay + reverb meta-effects. Toys are assembled from these. *(Forces: Operator-authoring contract is now concrete from the MVP; this is mostly mechanical, parallelizable.)*
- **Tonal-context / harmony bus** (ADR-0008) — scale broadcast + snap-to-scale + chord-progression publishing; followers (arp, voicing, melody) subscribe. Makes "always in key" mechanical, not hope. *(Forces: tonal-context bus mechanics — grill first.)*

### V1.2 — Playable surface

- **Performance-input mapping** — how gestures (tap-to-play, drag/strum, XY pad, controller) map to Messages. The bridge from UX to engine. *(Forces: playable-surface thread — grill first.)*
- **Curated control surface** — an Instrument exposes a public set of good-buttons over its structural addresses (the "easy to learn" half of the gradient).

### V1.3 — The Toys

- **Groove box, tap-to-play chord/melody, drag/strum instrument, meta-effects** — built from V1.1 operators over the V1.2 surface. The payoff: instant music for beginners. *(Forces: Toy-design thread — grill per Toy.)*

### V1.4 — Good-button UX layer

- The surface a human actually touches. Driven over OSC first (TouchOSC / web stand-in), proving the control-surface API before any native GUI commitment.

### V1.5 — Reach & robustness (parallelizable; ship)

- **Lock-free live graph hot-swap + per-operator state preservation** — edit an Instrument without dropping audio (live authoring). Swap-from-empty already works; this is the in-place case.
- **External OSC I/O (out)** — the lingua franca crossing the process boundary outward.
- **MIDI I/O** — drive external gear/synths (boundary adapter; core stays OSC-only, ADR-0007).
- **Ableton Link / MIDI clock / OSC tempo sync** — feed the Clock (boundary adapters).
- **n-channel input and output**, with easy defaults.
- **Parallel executor** (lock-free worker pool) behind the existing trait — built when the serial executor stops keeping up, not before.
- **Linux (lead) + Windows builds.**

### V1.6 — Agent skills

- **Developer skill** — scaffold a new Operator (Rust + descriptor + tests).
- **Patcher skill** — build/modify Instruments and Rigs via the JSON schema + introspection API. *(Forces: introspection/query API shape — grill first.)*

## Later (post-v1)

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
