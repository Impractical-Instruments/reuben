# Why: A Message landing mid-block takes effect at its exact frame without operators tracking sample offsets.

[Rule](../../execution-runtime.md#sample-accurate-timing)

Messages carry sample-accurate timetags, and a Message may land mid-block — a note-on at sample 37
of 128. That timing must be honored *without* forcing single-node authors to juggle sample offsets,
which would destroy their authoring simplicity and AI-authorability. So delivery is hybrid, split by
what the port carries. A **float control/param** update is materialized into the input buffer at
its exact frame: fed by sparse messages, a literal, or nothing, the engine fills a scratch buffer
from the latched current scalar and writes the mid-block change in at its frame — so the author just
reads a buffer and sample-accuracy is automatic, no `process()` re-slicing. Held/event shapes that
cannot ride a float buffer (an `Enum`, `Harmony`, a `Note`) keep **block-slicing**: the engine
splits the block at the change boundary, runs 0–37, applies the value, runs 37–128; the author reads
"my current value" and never sees the offset. Event-oriented operators (sequencers, the Voicer, note
logic) are the *only* ones handed raw `(offset, payload)` lists — exactly the ones that reason in
events and want the offsets.

This decouples timing precision from block size: large, efficient blocks still get tight timing.
Block-rate-only delivery was rejected because it quantizes timing to the block (~2.7 ms at
128/48 kHz) — grooves feel loose; raw offsets for every operator (the VST3/CLAP model) was rejected
because it makes every author re-implement timing, killing single-node simplicity. Determinism is
preserved because slice points are a deterministic function of Message offsets
([deterministic-render](deterministic-render.md)). The consequence authors must handle: `process`
may be invoked several times per block, so an operator must tolerate arbitrary sub-block lengths.

One boundary worth naming: sample-accurate timing comes from *inside* the graph (the Clock and
emitted Messages), not from the external input queue. Messages arriving from outside — UDP, a
worklet `postMessage` — are applied at the start of the next rendered block, **block-quantized by
design**: their arrival jitter dwarfs sample resolution, so stamping them to a finer frame would be
fake precision. Internally, materialize writes do *not* split the block (the value is written into
the buffer at its frame); only held-shape changes split it, each interior change frame becoming a
segment boundary, since a held value must read as constant within one `process` call.

(The materialize-vs-slice split and the per-port latch it rests on are refined by the unified
`Message`/`Arg` model; the durable runtime position — mid-block Messages take effect at their frame,
transparently to the author — is what this rule fixes. The latch itself is
[latch-service](latch-service.md).)

Distilled from: ADR-0011
