# Boundary and threading: single-writer Coordinator, read-only Render

## Context

The outside world (audio device, OSC/MIDI/Link adapters, GUI, agents) must interact with the realtime Render core (ADR-0009) without ever causing it to allocate, block, or see torn state. This boundary is also the "removable native layer" seam, and must let the same core embed in a game engine or DAW (ADR-0001 pluggable executor, ADR-0007 OSC-only).

## Decision

Three regions:

- **Render region (hard RT).** The audio device callback *hosts* Render of the current Plan on the audio thread, dispatching parallel clusters to the executor pool (ADR-0001). It only ever **reads** an immutable Plan; it never allocates or frees.
- **Coordinator region (non-RT).** The **single writer** of graph structure. Owns the canonical graph and the Instrument library, receives edit commands, performs Instantiate + Swap (ADR-0009), and runs deferred reclaim. Home of (de)serialization (ADR-0004).
- **I/O & control region.** The audio device (clocks Render); OSC/MIDI/Link boundary adapters (ADR-0007); the GUI/app/agents.

Everything crosses the RT boundary by **lock-free queue**:

- **Params/control** → a Message queue Render drains each block. No Swap — a knob turn never reaches the Coordinator.
- **Structural edits** → a command queue to the Coordinator → it Instantiates a new Plan → hands it to Render for the atomic Swap → Render publishes the retired Plan to a reclaim queue the Coordinator frees off-thread.
- **Render → outside** → a queue for metering, levels, emitted Messages, and introspection state, so the GUI and agents can observe a live system (ADR-0004).

**Invariant: one writer of structure (Coordinator), one reader at Render (immutable Plan), everything else lock-free message passing.** No shared mutable state crosses the RT boundary — enforced by Rust's `Send`/`Sync`.

**The removable-native-layer line:** the I/O region and the executor pool are native/removable; the Render core and Coordinator are portable. Embedding swaps the device-driven callback for the host's tick and the worker pool for the host's job system; the Coordinator and queues are unchanged.

## Consequences

- Introspection is served from the Render→outside queue, not by reaching into Render.
- The Coordinator is the natural home for the library, serialization, and Swap policy.
- Render correctness reduces to "read an immutable Plan, drain lock-free queues" — small and auditable.
