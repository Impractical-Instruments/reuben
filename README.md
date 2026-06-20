# reuben

A configurable musical instrument built from composable **Operators** that each do something simple and combine into complex musical behavior. Easy for beginners via ready-made **Toys**; deeply customizable once you get the hang of it. Rube Goldberg machines, for music — hence "reuben."

Music is the primary payload, but the same data (notes, chords, timing, gestures) can drive anything controllable over time — lights, video, game engines. **OSC is the lingua franca**, internally and externally. n-channel in and out. Easy defaults always provided. Ships Linux (lead) + Windows; the native layer is fully removable and the library is portable to mobile, the web, and game engines.

## Start here

- **[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)** — the design, end to end.
- **[CONTEXT.md](CONTEXT.md)** — the glossary / ubiquitous language. Read this first if a term is unclear.
- **[ROADMAP.md](ROADMAP.md)** — what's MVP, v1, later, someday, and never.
- **[docs/adr/](docs/adr/)** — the architectural decisions and the reasoning behind them.
- **[docs/OPEN-QUESTIONS.md](docs/OPEN-QUESTIONS.md)** — the design backlog: decisions not yet made.

Status: **design phase.** No engine code yet — see the roadmap for the MVP spine ("it makes a sound" headless core, driven over OSC from TouchOSC/Max).
