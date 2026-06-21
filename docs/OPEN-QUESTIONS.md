# Open Design Questions

Decisions not yet made. This is the design backlog — distinct from [ROADMAP.md](../ROADMAP.md) (feature priority) and [docs/adr/](adr/) (decisions already made). Each entry has enough context to resume a grilling session cold. None blocked the (now-complete) MVP; several are the threads the [v1 plan](../ROADMAP.md#v1-first-real-release) flags for a grilling session before their phase starts.

**Recently grilled → decided:** Tonal-context / harmony engine mechanics — transport, latched read, the context Operator, scale/chord representation, snap-to-scale, and sample-accurate timing. See [ADR-0013](adr/0013-tonal-context-bus-mechanics.md) and the [worked examples](tonal-context-examples.md). Also the **sample player + resource store** — sample-data references in the JSON format, the central `ResourceStore`, the `bind_resources` two-phase-init seam, the WAV resolver, and degrade-to-silence failure. See [ADR-0016](adr/0016-sample-player-and-resource-store.md).

## Major threads (ungrilled)

- **Playable surface / performance input.** How an Instrument exposes its playable surface and how input gestures map to Messages: tap-to-play chords/melodies, drag/strum, XY pads, controller mapping. The heart of the "good button" experience and the bridge from engine to UX.
- **Operator authoring (Rust).** The concrete authoring contract: the Operator trait, the descriptor macro/derive, the process signature (how a single-Lane operator receives input blocks + current param values + event lists and writes output), state ownership. Grounds the AI operator-authoring skill and the MVP's ~5 operators.
- **Audio + n-channel device layer.** Native device I/O: sample-rate and block-size negotiation, mapping the n-channel model onto real device channels, and xrun/dropout policy (what Render does when a block misses its deadline).
- **Toy design.** What the launch Toys are and how they're built from Operators — groove box, tap-to-play chord/melody players, drag/strum instruments, meta-effects — and what makes each one "good button."
- **Format and library.** *(Partly decided: sample-data references + the resource-load step landed with [ADR-0016](adr/0016-sample-player-and-resource-store.md) — a top-level `resources` table, a `ResourceResolver` seam, dedup at load.)* Remaining: the referenced-**Instrument** library (resolution, naming, versioning); a configurable sample-root and non-file sources; how formats and saved Instruments/Rigs migrate as the system evolves; and save round-tripping a resource binding (a built `Graph` does not retain the logical id today).
- **Introspection / query API.** The shape of the API agents use to explore a live system (list Operators, inspect descriptors, traverse a graph). Flagged as "likely needed" in [ADR-0004](adr/0004-ai-authorability-first-class.md).

## Smaller parked items

- **Block-slicing granularity** — global vs per-operator/cluster ([ADR-0011](adr/0011-message-delivery-and-timing.md)). A Render-internal optimization; contract is settled either way.
- **Buffer-pool ownership** across cluster boundaries (cache locality, who owns edge buffers).
- **Scheduling policy** — static core-pinning vs dynamic work-stealing for the executor.
- **Cycle delay granularity** — feedback unit delay of one block vs one sample.
- **Engine start/stop** lifecycle details (acquiring the audio device, running the Render thread) — noted as orthogonal to Plans in [ADR-0009](adr/0009-graph-lifecycle.md).
