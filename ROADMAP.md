# reuben — Roadmap

Living priority buckets. Maintained during design sessions. An item moves down a tier only by an explicit decision, not by drift.

## Tiers

- **Now (MVP / spike)** — the smallest end-to-end slice that proves the engine: a graph that makes sound. Throwaway-friendly.
- **v1 (first real release)** — the actual product people use. Linux + Windows, Toys, good-button UX.
- **Later (post-v1)** — real intent, not yet scheduled.
- **Someday / Maybe** — exploratory; may never happen. Parked so it stops competing for attention.
- **Non-goals** — explicitly will not do. Recorded to prevent re-litigation.

---

## Now (MVP / spike) — headless "it makes a sound" spine

Goal: exercise the whole spine end-to-end as cheaply as possible, then get out of the way for UX/Toy work. Drive it from TouchOSC (stand-in GUI) and Max (test harness) over OSC — no GUI.

- Rust workspace: portable `core` crate + `native` crate (audio out via `cpal`) — the ADR-0012 seam from line one
- Signal + Message types; topological **Plan**; Instantiate → Render loop; single-Lane fan-out (ADR-0010)
- Determinism invariant baked in from the start (ADR-0001)
- Executor *interface* present, **single-threaded implementation first** (parallel pool deferred)
- ~5 operators: oscillator, envelope, filter, Voicer, output sink
- **OSC-in boundary adapter (UDP)** — drive from TouchOSC / Max / scripts (ADR-0007)
- Load one JSON Instrument → Instantiate (Swap-from-empty) → Render → audio out
- Default 12-TET Tuning only; symbolic-pitch seam present (ADR-0008)

## v1 (first real release)

- Parallel executor implementation (lock-free worker pool) behind the existing interface
- Lock-free live graph hot-swap + per-operator state preservation
- External OSC I/O (the lingua franca crossing the process boundary)
- MIDI I/O; drive external gear/synths (boundary adapter — core stays OSC-only, ADR-0007)
- Ableton Link / MIDI clock / OSC tempo sync, feeding the Clock (boundary adapters)
- n-channel input and output, with easy defaults
- Toys: groove box, tap-to-play chord/melody, drag/strum instrument, meta-effects
- Good-button UX layer
- Linux (lead) + Windows builds
- Agent skills — developer: scaffold a new Operator (Rust + descriptor + tests)
- Agent skills — patcher: build/modify Instruments and Rigs via the JSON schema + introspection API

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
