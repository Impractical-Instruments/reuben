# Unified block-based graph with a static parallel schedule

## Context

reuben is one dataflow graph mixing non-audio data (notes, chords, timing, gestures, OSC) and audio. Past attempts stalled because standing up a bespoke audio engine consumed all the time. The graph must run realtime, exploit multiple cores, and stay portable enough to embed in game engines and other realtime hosts, with the native layer fully removable.

## Decision

- **One unified graph, processed in blocks.** Each block: message-domain data (discrete, timestamped, OSC-shaped) and signal-domain data (audio-rate float buffers) are computed in a single dependency-ordered pass — not two separate phases. There is no separate control-rate signal; sub-audio-rate control travels as Messages (Max/PD model).
- **Single static topological schedule.** Execution order is one topological sort over the whole graph. Computed once when the graph changes, not per block. A signal→message consumer (e.g. an envelope follower converting audio to control values) sees the current block's audio because topo order visits the producer first. True feedback cycles are broken with a unit delay; only real cycles pay that delay.
- **Static parallel plan.** The schedule is a DAG: independent branches run concurrently. Nodes are coalesced into **cost-weighted clusters** so task-dispatch overhead is amortized (no per-node tasks).
- **Pluggable executor — reuben does not own threads.** The core emits a parallel execution plan (tasks + dependency edges) and dispatches through an executor interface. The standalone native layer ships a lock-free, pinned, allocation-free worker pool; embedded hosts (game engines, DAWs) map tasks onto their own job system. The executor lives in the removable native layer.
- **Determinism is a hard invariant.** Output is bit-identical regardless of thread interleaving. Fan-in (mixers, summing buses) uses fixed reduction order; feedback uses unit delay.

## Considered and rejected

- **Two-phase (all control, then all audio):** simpler, but every audio→control edge eats a full block of latency even when acyclic.
- **Demand-driven interleaving:** tighter feedback, but runtime bookkeeping and sub-block splitting that are hard to keep realtime-safe.
- **Core-owned thread pool:** fights host job systems on embed; oversubscription and priority inversion.

## Consequences

Live graph mutation requires a lock-free hot-swap of the plan (build off-thread, atomic swap at a block boundary, deferred reclamation) and preservation of per-operator state across recompiles. Those are tracked as their own decisions.
