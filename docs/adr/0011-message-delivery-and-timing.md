# Message delivery and sample-accurate timing

> **Amended by [ADR-0028](0028-one-input-shape.md).** Block-slicing now serves only the
> scalar/event shapes (`Enum`, `Harmony`, `Note`), whose reads need a sub-block boundary. A
> `Float` param/control update is no longer applied by slicing — it is **materialized into the
> input buffer at its frame** (sample-accurate, one `process()` call). The hybrid split below
> (param/control vs event) survives, reframed in terms of shape.

## Context

Messages carry sample-accurate timetags (ADR-0001, ADR-0006). During Render, an operator processes a block; a Message may land mid-block (e.g. a note-on at sample 37 of 128). We must honor that timing without forcing single-Lane operators (ADR-0010) to juggle sample offsets, which would destroy their authoring simplicity and AI-authorability.

## Decision

Hybrid delivery, split by what the port carries:

- **Param/control Messages → engine block-slicing (sample-accurate, author-transparent).** The engine splits the block at Message boundaries: for a value arriving at sample 37, it runs the operator over 0–37, applies the value, runs 37–128. The single-Lane author writes `process(block)` and reads "my current value" — never sees the offset, but the change lands exactly there. This decouples timing precision from block size: large, efficient blocks still get tight timing.
- **Event Messages → raw timetagged lists for event-oriented operators.** Sequencers, the Voicer, note logic, etc. receive a time-ordered list of `(offset, payload)` and honor offsets themselves, because they reason in events and *want* the offsets.
- **Param smoothing** (declared in the descriptor, ADR's Q8 operator contract) handles zipper noise on continuous changes — a separate concern from slicing, which handles accuracy.
- **Determinism preserved:** slice points are a deterministic function of Message offsets (ADR-0001 invariant).

The choice between **global slicing** (whole graph steps through the same sub-blocks; simple, keeps parallel clusters aligned; reprocesses unaffected operators) and **per-operator/cluster slicing** (only affected operators slice; more efficient under dense messaging; trickier with parallelism) is a Render-internal optimization deferred to build time. The operator contract is identical either way.

## Considered and rejected

- **Block-rate only** (apply all Messages at the block boundary): simplest, but quantizes timing to block size (~2.7 ms at 128/48 kHz) — drums and arps smear, grooves feel loose.
- **Raw offsets for all operators** (the VST3/CLAP model): maximally flexible, but every author re-implements timing — kills single-Lane simplicity and AI-authorability.

## Consequences

- An operator's process may be invoked multiple times per block; operators must handle arbitrary sub-block lengths.
- Event operators are the only ones exposed to raw timetags — exactly the ones that should be.
