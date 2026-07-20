# Why: One unified graph of message and signal data runs in fixed-size blocks, each block computed in a single dependency-ordered pass.

[Rule](../../execution-runtime.md#unified-block-graph)

reuben is fundamentally *one* dataflow graph carrying both non-audio data (notes, chords,
timing, gestures, OSC) and audio, not two engines bolted together. Keeping them in one
block-based, dependency-ordered pass — the Max/PD model, with no separate control rate — is what
lets a signal→message consumer (e.g. an envelope follower turning audio into a control value) see
the *current* block's audio: topological order visits the producer before the consumer within the
same block, so an acyclic audio→control edge costs nothing.

The rejected alternative — two phases, all control then all audio — is simpler but makes every
audio→control edge eat a full block of latency even when it is acyclic; drums and arps smear.
Demand-driven interleaving would tighten feedback but needs runtime bookkeeping and sub-block
splitting that are hard to keep realtime-safe. The one genuine split kept on day one is dense
audio buffers, which cannot be 48k OSC packets per second — that concession is sound and stops
there; everything else stays one graph.

See also [static-parallel-schedule](static-parallel-schedule.md) (how the pass is ordered and
parallelized) and [deterministic-render](deterministic-render.md) (the invariant it must hold).

Distilled from: ADR-0001
