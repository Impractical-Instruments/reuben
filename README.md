# reuben

A configurable musical instrument built from composable **Operators** that each do something simple and combine into complex musical behavior. Easy for beginners via ready-made **Toys**; deeply customizable once you get the hang of it. Rube Goldberg machines, for music — hence "reuben."

Music is the primary payload, but the same data (notes, chords, timing, gestures) can drive anything controllable over time — lights, video, game engines. **OSC is the lingua franca**, internally and externally. n-channel in and out. Easy defaults always provided. Ships Linux (lead) + Windows; the native layer is fully removable and the library is portable to mobile, the web, and game engines.

## Start here

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — the design, end to end.
- **[CONTEXT.md](CONTEXT.md)** — the glossary / ubiquitous language. Read this first if a term is unclear.
- **[ROADMAP.md](ROADMAP.md)** — what's MVP, v1, later, someday, and never.
- **[docs/adr/](docs/adr/)** — the architectural decisions and the reasoning behind them.
- **[docs/OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md)** — the design backlog: decisions not yet made.

Status: **playable live instrument.** The portable core (`crates/reuben-core`) makes a verifiable, deterministic tone offline — Signal/Message data model, Operator trait + descriptors, Graph → Plan (Instantiate) → block-sliced serial Render, five operators (oscillator, envelope, filter, monophonic voicer, output), 12-TET tuning. The removable native layer (`crates/reuben-native`) now plays it live: `cargo run -p reuben-native --bin reuben` opens the default audio device and listens for OSC on UDP `0.0.0.0:9000`. Play notes by sending `/voicer/note [midi, gate]` (e.g. `[69.0, 1.0]` for note-on A4, `[69.0, 0.0]` for note-off) from any OSC source. Offline: `cargo run -p reuben-core --example first_sound` writes `first_sound.wav`. Next: polyphony + per-Voice fan-out, JSON-defined instruments, sample-accurate OSC timing — see the roadmap.
