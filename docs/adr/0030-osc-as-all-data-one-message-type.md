# OSC-as-all-data: one `Message` type, an `Arg` payload, `Signal` as a Buffer-arg

## Status

Accepted (2026-06-25). Supersedes [ADR-0028](0028-one-input-shape.md) (shapes / the
`Delivery × Data` two-axis model) and the carrier doctrine of
[ADR-0017](0017-playable-surface-and-control-domain.md). Amends
[ADR-0011](0011-message-delivery-and-timing.md) (block-slicing now serves the unified latch),
[ADR-0014](0014-internal-message-graph.md) (routing unifies), and
[ADR-0015](0015-latched-context-read.md) (the Harmony lane folds into the latch).

## Context

[ADR-0007](0007-osc-only-core.md) set the north star: *the core speaks only OSC-shaped
Messages.* [ADR-0001](0001-unified-block-graph-execution.md) conceded the one necessary split
on day one — dense audio buffers cannot be 48k OSC packets/second — and that concession is
sound. Everything *after* drifted away from the star. [ADR-0028](0028-one-input-shape.md)
reframed data as **"shapes,"** and the engine accreted **seven distinct internal carriers**: a
dense f32 arena, a sparse emit pool, a separate `Harmony` `Copy`-struct arena with its own
resolver lane, an enum latch, a param lane, materialized-float buffers, and the outbound lane.
Harmony, enum, and param each became their own non-OSC mechanism. The two-axis
`Delivery × Data` model proposed in handoff doubled down — it *typed* the divergence instead of
removing it.

The insight that collapses it: most of those carriers are not different *kinds of data* — they
are different *read styles over one thing*. A held enum is "the last `/mode` message's arg."
Harmony is "the last `/harmony` message's args, decoded." A control float is "zero-order-hold of
the last `/cutoff` message." One carrier — an OSC-shaped Message stream — read three ways. The
f32 buffer is only the dense *representation* of a float stream, kept for performance.

This is pre-alpha; breaking changes are explicitly acceptable. The goal is the architecture we
want *before* wider use.

## Decision

**One type — `Message = { address, timestamp, Arg }` — perf-friendly by staying close to the OSC
spec without its binary representation.**

### The model

1. **`Message` carries exactly one `Arg`.** This is a deliberate divergence from OSC (which
   allows many args). It is *why* concrete-type Args exist: two scalars (pitch + velocity) cannot
   be two args, so they pack into one `Arg::Note`. The `address` is kept for OSC shape, boundary
   routing, and debug — **never** internal dispatch. The `timestamp` is an internal sample
   `frame` (not OSC-spec, a deliberate divergence); incoming external OSC has no timestamp and is
   stamped frame 0 ("now"). OSC **Bundles** (timetag + grouped sub-messages) are reserved for
   genuine multi-event grouping (chords), adopted later — not for single-message timing.

2. **`Arg` is one closed, central enum**: OSC primitives (`F32`/`I32`/`Str`), shared *vocab*
   concrete types (`Note`, `Harmony`, `FilterMode`, `Waveform`, …), and the optimized dense
   payload (`Buffer`). The concrete types are **shared domain vocabulary**, defined once and
   reused everywhere — which is what lets a *closed* `Arg` enumerate them (a `FilterMode`
   duplicated per-operator would be the code smell). Enums read as real Rust enums in operator
   code (`FilterMode::HighPass`), not bare indices.

3. **`Signal` is a `Message` whose `Arg` is a `Buffer`** — shorthand, not a second type.
   `Buffer` is the contiguous-memory payload, represented the most performant way. `Signal<f32>`
   is the only kind today, but the model is architected so other element kinds can exist
   (`Signal<T>`) without building that generality now.

4. **A held value is the zero-order-hold of a port's last `Arg`.** The Harmony arena, enum
   latch, and param lane all collapse into **one per-port last-Message latch**. Harmony's
   resolver methods stay on the `Harmony` type (`Copy`); the latch stores a `Copy`-normalized
   Arg (a held enum holds its resolved value, never a `String`, to stay allocation-free).

5. **One implicit bridge, and only one.** An `F32`-source wired into a `Buffer` port
   ZOH-materializes (step-and-hold into the block buffer at the change frame) — "a single f32
   Message auto-converts to a Signal if the operator calls for it." There is **no** auto
   `Signal → Message` (a `Buffer`-source into a scalar port is illegal; it needs an explicit
   sampler op), and **no** implicit message-rate → signal-rate smoothing (ZOH step is automatic;
   slew / glide / smoothing is always an explicit shaper op). `m2s` narrows to exactly that
   shaper — its old Snap mode is now the wire's automatic ZOH.

### Read / write API

Reads unify to two generic verbs: `io.stream::<T>(port)` (iterate each Message's typed payload;
a `Buffer` port yields one item per sub-block) and `io.last::<T>(port)` (the ZOH most-recent
payload; a `Buffer` port returns the block `&[f32]`). Writes are two honest verbs:
`io.emit(port, payload, frame)` (append a sparse Message) and `io.signal_mut(port)` (fill this
node's own output buffer in place). The buffer model is per-edge (SSA-ish): within a node in ≠
out (disjoint memory via the `out_scratch` swap); across an edge the consumer's input *is* the
producer's output (zero-copy borrow, fan-out = N readers); buffer reuse across non-overlapping
edge lifetimes is the plan's job, not the operator's.

### Boundary

External OSC routes by address to a node/port; the **port's declared Arg type** drives
conversion (a primitive port wraps the single arg; a vocab port calls `T::from_osc(args)`) —
single source of truth is the descriptor, no separate registry to drift. The external form is
flat multi-arg (`Note ↔ /note pitch vel`), derive-generated. Opt-out is by *not* implementing
the OSC trait — `Buffer` does not, so audio cannot cross the boundary by construction.

## Considered alternatives

- **The two-axis `Delivery × Data` model** (the prior handoff). Rejected: it makes the seven
  carriers a *typed taxonomy* rather than removing them. "Held vs sparse vs dense" is a read
  style over one Message stream, not a second axis to declare.
- **`Arg::Blob` for Harmony / structs** (raw bytes, resolvers reconstructed on read). Rejected:
  loses human-readability and the compile-time data contract; concrete vocab types give both for
  free and keep `Copy`/alloc-free.
- **Per-operator enum types.** Rejected as a code smell (`FilterMode` is reused everywhere) and
  it would force `Arg` open. Shared *vocab* keeps `Arg` closed.

## Consequences

- **Breaking, engine-wide.** `Shape` is retired; ports carry an `Arg` type. `Io` exposes
  `stream`/`last`/`emit`/`signal_mut` in place of
  `signal`/`value`/`varying`/`enum_index`/`events`/`harmony`/`publish_harmony`/`send_outbound`.
  All 26 operators and 18 instruments migrate; the golden descriptor snapshot and generated
  instrument schema are re-blessed.
- **Seven carriers become one** Message stream plus one per-port latch. `context_arena`,
  `enum_latches`, and `params` collapse into the latch; `msg_targets` / `ctx_targets` / outbound
  routing unify into the one message path. Block-slicing survives as the sample-accurate
  mechanism for held-Arg changes that cannot materialize into an f32 buffer (Harmony, enum,
  Note); materialize survives for the `F32 → Buffer` bridge. *(2026-06-26)* The `input_latches:
  Vec<f32>` shadow lane — a perf duplicate of the latch, hand-synced in two places — is gone;
  `latch: Vec<Arg>` is the sole ZOH store, and the materialize fill decodes it via `Arg::as_f32`.
- **A new shared `vocab` module + `ArgValue` derive macro.** Defining a domain type = declare it
  in `vocab`, derive, add one line to `Arg`; the derive generates OSC `to/from`, `Arg`
  integration, and metadata. `Harmony` and `Pitch` move from `reuben-core` into `vocab`.
- **Terminology.** `Pitch` becomes `enum { Degree(i32), Absolute(f32) }` (no invalid states);
  `Note = { pitch, velocity }`, velocity 0 = note-off. `Shape` is retired in favor of "the
  port's Arg type." `Signal` is redefined as "a Message whose Arg is a Buffer." `CONTEXT.md` is
  updated accordingly.
- **Divergences from OSC we keep deliberately**, recorded so a future reader does not "fix"
  them: an internal timestamp, one Arg per Message, concrete-type Args (instead of blobs), and
  the Buffer payload.
- **Deferred:** a `vocab` `Trigger`/unit type for payload-less bangs; `Arg::Str` interning if a
  hot path appears; chord-as-simultaneous-notes via the reserved OSC Bundle; non-f32
  `Signal<T>` element kinds (architected-for, not built).
