# Latched context read: a struct-valued latch service over the Message wire

> **Amended by [ADR-0028](0028-one-input-shape.md).** "Context" becomes the **`Harmony`** shape.
> The latch + `Copy` resolver struct survive unchanged; the dedicated context arena/accessor folds
> into shape delivery (a held-struct discipline). Read it with `io.harmony(IN)`, publish with
> `io.publish_harmony(OUT, frame, h)` — the `io.context`/`io.publish_context` names persist as
> aliases until the struct itself is renamed.

## Context

[ADR-0013](0013-tonal-context-bus-mechanics.md) decided tonal context is an Operator
holding a latched struct + resolver, read by followers as "what's the chord right now."
It described the transport as "Messages over wildcard dispatch" and the latch as "the param
block-slicing path, with a structured value instead of an f32." Both descriptions predate
what actually got built ([ADR-0014](0014-internal-message-graph.md)) and need grounding:

- ADR-0014 built the **wired** Message edge (`sequencer.notes → voicer.notes`), resolved at
  Instantiate, delivering zero-copy `Event`s to `io.events()`. Wildcard/address dispatch is
  explicitly deferred. So context's transport is the wired edge, not wildcards.
- Block-slicing today is **f32-only** and **external-input-only**: a node's slice boundaries
  (`render::process_node`) come from `route.params`, filled solely from external block
  Messages. Emitted Messages (ADR-0014) drain into `routes[dst].events` — they never create
  a slice boundary. Context is a *struct* whose changes are *emitted* by an upstream
  publisher, so neither half of the existing param path covers it.

This ADR settles the read-side mechanics ADR-0013 left as analogy: where the latch lives,
how a struct rides the wire RT-safely, and how an upstream node's changes re-slice
downstream followers sample-accurately.

### The unified-wire reframe this rests on

Grilling the design surfaced that there is fundamentally **one edge** with two payloads:
**continuous** (an audio buffer — what we call a *Signal*) and **discrete typed** (an
OSC-shaped *Message*). Over the Message wire the engine offers **read services**:

- `events` — raw discrete delivery (the Voicer times itself).
- `param` — a latched **f32**: engine caches last value + block-slices so the author reads a
  constant, plus good-button metadata (a knob).
- `context` (this ADR) — a latched **struct**: the same latch+slice service, struct-valued,
  with a resolver instead of a knob.

`param` and `context` are **not separate wire types** — they are latch *services* layered on
the one Message wire. `Signal` is the *other payload*. Context is the struct-valued sibling
of `param`; it is a third read **accessor**, not a third edge.

## Decision

### The latch is an engine-held slot the Operator writes through `Io` (read via `io.context`)

The context node is an **ordinary `Operator`** (preserving the uniform node model and
ADR-0004 authorability — an embedder can write a custom context type). It:

1. reads publisher writes via `io.events()` (the ADR-0014 wire — already built),
2. updates its authoritative latch in `&mut self` (last-write-wins per field), and
3. writes the current value to an **engine-held context-output slot** via a new write
   accessor `io.publish_context(port, frame, &ctx)` — the sibling of `io.output()` (Signal)
   and `io.emit()` (Message).

Followers read that slot via **`io.context(port)`**. The engine owns the buffer; the operator
writes through `Io`; downstream reads through `Io` — exactly parallel to the Signal arena and
the emit pool. This is a **third arena** (Signal buffers / emit pool / context slots) and a
write+read accessor pair: the real new surface.

Rejected — a special engine-level context node (engine owns the struct directly): trivially
reachable, but it forks the node model and kills custom contexts. Not worth it.

### Changes drive follower slicing via the emit→route drain (a third route lane)

The context node is upstream, so in topological order it runs **before** its followers and
can fill their routes before they execute — exactly how emitted `Event`s already reach
`routes[dst].events`. We generalize that drain into a third lane:

- `io.publish_context(port, frame, &ctx)` produces `(frame, snapshot)` entries (sibling of
  `emit`, which already carries a frame).
- The engine drains each publish into every downstream reader's new
  `route.context: Vec<(frame, ctx_idx)>` **and** contributes its `frame` as a slice boundary.
- `process_node` merges context-change frames into `bounds`, beside the param-frame merge.
- Per segment, `io.context(port)` returns the snapshot whose `frame ≤ seg_start`.

A chord change at frame 40 thus creates a slice boundary at 40: the frame-37 note reads the
old context, the frame-45 note the new — sample-accurate, per ADR-0011, the same timeline as
notes. **Same-frame tie:** the context node runs upstream of followers (topo order), so a
write at frame F is visible to a read at frame F (ADR-0013's downbeat-chord rule).

This also exposes a latent asymmetry: today an **emitted** Message can drive a downstream
`event` but not a downstream `param` slice (only external input can). The same third-lane
machinery is what would let an emitted Message drive a param sample-accurately — a
generalization this ADR enables but does not require.

### Realtime-safe by construction: `Context` is `Copy`, the heavy data is an immutable registry

The block-lifetime change-list stores `(frame, snapshot)`. If `Context` held a `Vec`/`Box`,
snapshotting would clone → allocate on the audio thread → break `tests/rt_safe.rs`. So the
struct shape is *forced* by the slicing model:

- **`Context` is `Copy`** (memcpy-snapshot, no heap):
  - `root: i32` — tonic **step** (absolute; spans octaves).
  - `scale: [i16; CAP] + len` — within-period **step** offsets (`[0,2,4,5,7,9,11]`). `CAP = 64`
    (covers raga/maqam/large-MOS scale *lengths*). `i16` per offset covers any period up to a
    32,767-EDO — ~27× past the 1-cent-resolution practical ceiling (1200-EDO).
  - `chord: { tag: ScaleRel | Absolute, offsets: [i16; CHORD_CAP] + len }`.
  - `tuning: TuningId` — a small enum / registry index (`TwelveTet | Edo(n) | Scala(idx)`),
    **never a box**.
- **Heavy/variable data** — named scales, Scala `.scl` step→Hz tables — lives in an
  **immutable `ScaleRegistry`** built **off-RT at Instantiate** (from JSON + Scala import),
  held by the `Plan`, never mutated during render. The registry holds the tuning's *full
  ladder* (period N, possibly hundreds of steps); `Context.scale` holds the small *selection*
  (K degrees) — distinct axes (N is huge and registry-side; K is small and capped).
- **Symbolic args resolve at write time.** `Arg::Sym("dorian")` → a `TuningId`/offset-list is
  resolved inside the context op's `process` (off the per-sample path) when a write event
  arrives, so reads stay pure.

The absolute step index `root + scale[d] + octave*period` is the one unbounded quantity; it is
a **resolver intermediate computed in i32/i64**, never a stored field.

### The resolver is a view over `Copy` value + registry; reads never allocate

`io.context(port)` returns a `ContextView<'a> { value: Context, reg: &'a ScaleRegistry }`
exposing `hz(pitch)` / `snap(pitch, policy)` / `chord_tone(n)` — pure arithmetic over the
`Copy` value plus a registry lookup. The Scale∘Tuning composition lives in this one place
(ADR-0013), so followers stay dumb (`io.context().hz(p)`) and single-Lane authoring (ADR-0010)
stays simple.

### The latch sits upstream of fan-out (shared across Lanes; dynamic-Voice-safe)

The context node is single-Lane, pre-Voicer (ADR-0014 emission rule). Its slot is **persistent**
across blocks (like `PlanNode.params`) and shared by every downstream Lane, so the frame-0
value is always present. With today's **fixed** Lane pool (`spawn()` is called only at
Instantiate, never at runtime), a per-follower self-latch would *also* be correct — so the
engine latch is justified by **authoring simplicity + sample-accuracy done once**, not by the
spawn-reset argument ADR-0013 gave (see ADR-0013 amendment). It additionally **future-proofs**
dynamically-spawned Voices: a Lane born mid-stream reads the shared upstream slot and sees
current context instantly, where a per-Lane self-latch would be empty.

## Considered and rejected

- **Self-latch in each follower from raw `events`** (no engine latch): correct under the
  current fixed-Lane model, but pushes event→cache→per-sample dispatch into every follower
  (re-deriving the Voicer's change-point walk), loses the "read a constant" authoring contract,
  and breaks under dynamic Voices. The engine latch is the same trade as `param`.
- **A `Context` holding `Vec`/`Box`** (natural OO shape): snapshot clones allocate on the audio
  thread. Rejected for the `Copy` + registry split.
- **Address/wildcard dispatch as the context transport** (per ADR-0013's original wording):
  every change re-enters a String-keyed O(nodes) match. The wired edge is the cheap common
  case (ADR-0014); wildcards layer on later for the boundary, not under the audio-rate latch.
- **Route context changes up front** (before running the context node): impossible — the
  changes depend on the node's inputs. Delivery must interleave with topological execution,
  like emitted Messages.

## Consequences

- `Io` gains `publish_context` (write) + `context` (read); the `Renderer` gains a **context
  arena** (persistent slots) and a **third route lane** (`route.context`) fed by the emit→route
  drain; `process_node` merges context-change frames into `bounds`. Signal-only and
  param-only operators are unaffected.
- A `Context` value type (`Copy`), a `ContextView` resolver, and an immutable `ScaleRegistry`
  (named scales/tunings + Scala import), built at Instantiate and held by the `Plan`.
- `tests/rt_safe.rs` extends to a context-driven rig (publisher → context → resolving Voicer):
  steady state stays allocation-free, including across context changes (memcpy snapshots).
- The latent **emitted-Message → param-slice** generalization becomes mechanically available
  (same third lane), to be taken up when an operator needs it (e.g. an LFO driving a param
  sample-accurately over the message graph).
- Terminology, fixed for all downstream docs: **frame** = sample offset in a block (time);
  **step** = a rung in the tuning's pitch ladder (pitch). The `[i16]` offsets are pitch steps,
  never sample counts.
