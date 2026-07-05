# OSC-as-all-data: one `Message` type, an `Arg` payload, `Signal` as a Buffer-arg

## Status

Accepted (2026-06-25). Supersedes [ADR-0028](0028-one-input-shape.md) (shapes / the
`Delivery × Data` two-axis model) and the carrier doctrine of
[ADR-0017](0017-playable-surface-and-control-domain.md). Amends
[ADR-0011](0011-message-delivery-and-timing.md) (block-slicing now serves the unified latch),
[ADR-0014](0014-internal-message-graph.md) (routing unifies), and
[ADR-0015](0015-latched-context-read.md) (the Harmony lane folds into the latch).
Partially superseded by [ADR-0031](0031-float-resolves-to-value-or-signal-by-wiring.md) (a numeric
port declares a held Value or a Signal form) and amended by
[ADR-0037](0037-typed-port-handles.md) (typed port handles replace the `input::<T>`/`output::<T>`
verbs).

## Amendment (2026-06-30): vocab enums type-erase to `Arg::Enum(index)`

The original decision (point 2) gave **every** shared vocab type its own named `Arg` variant —
`Arg::FilterMode`, `Arg::Waveform`, `Arg::GateMode`, … one per enum. That re-introduced, in
miniature, the very coupling this ADR set out to remove: adding a vocab enum meant editing three
central engine sites — the `Arg` enum (`message.rs`), the outbound `osc_out_args` match
(`boundary.rs`), and the `impl_input_held!` list (`operator.rs`) — none of which care *which*
enum it is. The closed `Arg` named concrete vocab the engine never inspects by name.

**Resolution.** Vocab **enums** collapse to a single `Arg::Enum(u32)` variant carrying the bare
variant **index**. Type identity moves out of the value and into the **port descriptor**'s
[`EnumMeta`] — which is already the inbound authority (the port's declared type drives
`osc_in_arg`, per the Boundary section), so the value no longer needs to carry type redundantly.
This is the same port-authority the original ADR applied to the inbound side, now applied to
storage. Adding a vocab enum is purely local again: declare it in `vocab`, `#[derive(ArgValue)]`,
done — the derive generates the `From`/`TryFrom` index pack/unpack, the `EnumMeta`, and the
held-Value `IoInput` impl, so the enum self-registers and **no** central engine file is touched.

Unchanged: the **operator API still reads real Rust enums** — `io.input::<FilterMode>(port)`
returns a `FilterMode`, decoded via `FilterMode::from_index`. The erasure is a storage detail
below that surface. **Structs** (`Note`, `Harmony`) keep their own named variant: they carry a
real per-type shape (a `Note` is pitch + velocity, not an index), so there is nothing to erase.

Hot path stays `Copy` + allocation-free: a latched read is `Arg::Enum(i)` → `from_index(i)` (no
`String`, leaner than a symbol compare). The bare index cannot mis-decode because a latch slot
only ever holds its own port's enum (port-authority) and the operator names the concrete type at
the read site. No operator declares an enum **output** port, so a bare index never crosses a wire
ambiguously. **Known gap:** an enum leaving over OSC-out (`osc_out`) has no port context at the
boundary to recover its symbol, so it currently serializes as its bare index. *(2026-07-01)*
`osc_out` now forwards **any** `Arg` verbatim through its type-agnostic `arg` pass-through input
(issue #141), so an outbound enum can reach the boundary at all; symbol-on-the-wire still needs
the sink's wired source-port `EnumMeta` resolved at the engine drain — tracked as
[issue #147](https://github.com/Impractical-Instruments/reuben/issues/147) (drain-side
source-port resolution). Relatedly, [ADR-0035](0035-constants-are-immutable-ports.md)'s
save path already resolves enum overrides to symbols via the port's `EnumMeta`, not the value.

## Amendment (2026-07-01): the `arg` pass-through — opaque payloads may be untyped

Issue #141's fix added the first port with no declared `Arg` type: `arg`, the type-agnostic
pass-through (`osc_out.in`). The rule that keeps it from being a hole in the typed model:

**A port may be type-agnostic iff its operator treats the payload as opaque** — forward it,
buffer it, count it, drop it, but never *interpret* it. The moment behavior depends on the type,
the port must declare it. `arg` is therefore not a per-sink special case but the type for **pure
carriers**, of which `osc_out` is the first; an operator that computes over its input (a math op)
can never declare it — type-from-wiring for interpreting operators would mean dynamic dispatch on
the render thread or plan-time monomorphization, neither of which this buys.

Port-authority is *delegated*, not absent: the wire is monotyped in practice, and the **wired
source port** is its type authority (the same authority issue #147 reads for outbound enum
symbols). Three fences keep the delegation sound:

1. **Input-only.** The contract validator rejects an `arg` output or constant. An in-graph
   carrier (`arg` in *and* out) would need the plan to trace types *through* the carrier for any
   typed input downstream — machinery deferred until an operator earns it.
2. **Capability-keyed legality.** A source wires into `arg` **iff** its type has an external OSC
   form (`boundary::has_osc_form`, the single statement consumed by both the load-time and
   plan-time checks): the primitives, vocab enums (index today, per the gap above), and `Note`'s
   flat form. `Harmony` (no OSC form) and any Signal are rejected loud — a wire that could never
   send anything is a patching mistake, not a silent drop. *(2026-07-05)* The converters
   [issue #146](https://github.com/Impractical-Instruments/reuben/issues/146) promised have
   landed (issues #204/#205): a struct vocab type self-registers its inbound converter at its
   definition site (`register_osc_form!` → `boundary::OscForm`, the ADR-0024 inventory pattern;
   `Note` is the first registrant), and `has_osc_form`'s struct arm reads the same registry.
   Outbound deliberately stays the closed exhaustive `osc_out_args` match over `Arg` — a runtime
   outbound registry over a closed enum was rejected — with a test pinning the two sides
   together. `Harmony` remains the legitimate no-form opt-out (it registers nothing); its
   external wire form is deferred to
   [issue #209](https://github.com/Impractical-Instruments/reuben/issues/209).
3. **Inbound is a single atom.** External OSC addressed at an `arg` port crosses only as one
   `F32`/`I32`/`Str` (the echo/loopback path): a multi-arg list has no unambiguous single-`Arg`
   form without a typed destination port. The string atom was originally excluded because
   forwarding it would heap-allocate on the render thread (ADR-0009); *(2026-07-05)* with
   `Arg::Str` backed by `Arc<str>` (issue #206) that forward is a refcount bump, so a single
   string atom crosses too (issue #207). Consequently the flat 2-arg Note form the sink *sends*
   still does not round-trip back in through an `arg` port — a typed `note` port still decodes
   it.

[`EnumMeta`]: ../../crates/reuben-core/src/descriptor.rs

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
   types (`Note`, `Harmony`, and — since the 2026-06-30 amendment — every enum erased to one
   `Enum(index)` variant), and the optimized dense payload (`Buffer`). The vocab types are
   **shared domain vocabulary**, defined once and reused everywhere (a `FilterMode` duplicated
   per-operator would be the code smell). Enums read as real Rust enums in operator code
   (`FilterMode::HighPass`) even though they **store** as a bare index — see the amendment above.

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
*(2026-07-05)* As built (epic #146): the struct-type conversion is a hand-written `OscArg` impl
beside the type, **self-registered** with the boundary via `register_osc_form!`
(`boundary::OscForm`, the ADR-0024 inventory pattern) and looked up by the port's declared type
name — port-authority holds, and self-registration means adding a converter edits no central
match. Outbound (`osc_out_args`) stays a closed exhaustive match over `Arg`, drift-guarded by
test. The struct opt-out is by *not registering*: `Harmony` does neither (wire form deferred to
issue #209).

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
  hot path appears *(2026-07-05: `Str` is `Arc<str>`-backed since issue #206, so a render-thread
  clone is a refcount bump; interning proper stays deferred)*; chord-as-simultaneous-notes via
  the reserved OSC Bundle; non-f32 `Signal<T>` element kinds (architected-for, not built).
