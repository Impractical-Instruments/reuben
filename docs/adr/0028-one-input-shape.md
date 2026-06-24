# One `Input`, one axis: `shape`. Density and delivery follow from it

## Context

An operator today receives data through **six** distinct concepts, several of which describe the
*same* input:

- a **Signal input port** (`io.input(port) -> Option<&[f32]>`, per-sample buffer),
- a **Message input port** (`io.events() -> &[Event]`, sparse timestamped),
- a **Context input port** (`io.context(port) -> Context`, a latched harmonic struct,
  [ADR-0015](0015-latched-context-read.md)),
- a **param** (`io.param(slot) -> f32`, a block-sliced scalar, [ADR-0011](0011-message-delivery-and-timing.md)),
- the **unwired default scalar** a Signal port falls back to when nothing is wired (the
  one-port-one-type rule, [ADR-0017](0017-playable-surface-and-control-domain.md)), and
- JSON-only **`control`** metadata ([ADR-0018](0018-control-surface-generation.md), engine-ignored).

The duplication is structural. A filter's `cutoff` is declared **twice** in the contract — once
as a Signal `input`, once as a `param` — and read with a two-path expression
(`io.input(IN_CUTOFF).map_or(io.param(P_CUTOFF), |b| b[i])`).
[ADR-0017](0017-playable-surface-and-control-domain.md) half-noticed this ("two carriers, one
value") but kept **Message vs Signal as an author-chosen carrier** and minted `m2s` to bridge
them. [ADR-0015](0015-latched-context-read.md) noticed the other half — "param is a latched
`f32` with block-slice; **context is the same service, struct-valued, with a resolver instead of
a knob**" — i.e. the engine *already* treats param and context as one mechanism differing only by
the kind of value.

This ADR collapses the six into **one authored concept — `Input` — described by a single axis,
`shape` — plus a small `Constant` carve-out** for instantiate-time configuration. Everything
else (how densely the value arrives, how it is read, whether it can be held) **follows from the
shape**; none of it is a separate thing the author declares.

Raised as an architecture review of operator inputs and params.

## Decision

### An `Input` is just a `shape`; delivery follows from it

Every functional input an operator consumes is **one `Input`**, declared once, carrying one
piece of information — its `shape`, drawn from a **closed, named set** (not a generic struct
facility):

| `shape` | what it is | delivery discipline | read view(s) |
|---|---|---|---|
| **`Float`** | a number (freq, cutoff, amp, a contour, a control) | a per-sample value stream, materialized from a latched current scalar | `io.signal(IN) -> &[f32]` (+`varying`) **or** `io.value(IN) -> f32` |
| **`Enum`** | a named discrete choice (filter `mode`, osc `waveform`) | a held scalar, block-sliced on change | `io.enum(IN) -> E` |
| **`Harmony`** | the tonal-context struct: `root`/`scale`/`chord` + resolvers `hz()`/`snap()`/`chord_tone()` ([ADR-0008](0008-pitch-and-tuning.md), [ADR-0013](0013-tonal-context-bus-mechanics.md)) | a held struct, block-sliced on change | `io.harmony(IN) -> Harmony` |
| **`Note`** | a pitch/velocity event | a sparse, frame-stamped event list | `io.events() -> &[Event]` |

There is **no generic `Struct`**: `Harmony` and `Note` are specific built-ins, preserving the
`Copy`, allocation-free audio-thread guarantee ([ADR-0015](0015-latched-context-read.md)).

**There is no separate "temporality" axis, and no author-visible "carrier."** Whether a value is
latchable, dense, or sparse is *intrinsic to its shape*: `Float` is always a per-sample stream,
`Enum`/`Harmony` are always held scalars, `Note` is always a sparse event stream. The old
carriers map onto shapes and disappear as concepts — **Signal** = a `Float`'s buffer view;
**param** = a `Float` read as a scalar / a held `Enum`; **Context** = `Harmony`; **Message
events** = `Note`.

Rejected — *a `shape × temporality` model* (an earlier draft of this ADR): once `Float` is always
materialized into a buffer (below), "Level vs Stream" has **no runtime manifestation** for a
`Float` and is fake precision; and the other shapes each admit exactly one temporality. The
second axis collapses into the first.

Rejected — *a single flat `type` enum* (`Signal | Message | Enum | Float | Context | …`): each
variant secretly fixes both "what kind of value" and "how it's delivered," so it cannot express
that delivery is *derived*, nor reject illegal combinations.

### Defining a new shape: four disciplines, two cost tiers

The shape set is **closed but deliberately extensible**. The closed thing is not the shapes
themselves but the four **delivery disciplines** — the ways the engine carries and serves a
value (an arena + accessor + slicing rule). A discipline is the expensive, ADR-level unit
([ADR-0015](0015-latched-context-read.md) added one — the held-struct/context arena — as its own
ADR). Shapes live *inside* a discipline and are cheap to add.

| discipline | engine machinery | shape(s) today | room |
|---|---|---|---|
| **numeric stream** | per-sample buffer (+ scalar latch) | `Float` | none — audio is f32-only ([ADR-0017](0017-playable-surface-and-control-domain.md)) |
| **held scalar** | latched value, block-sliced | `Enum` | a bounded discrete type |
| **held struct + resolver** | latched `Copy` struct + methods, block-sliced | `Harmony` | e.g. a future `Transport` (musical position as a queryable value; clock emits `Float` phase today, so unneeded until a consumer wants it) |
| **sparse event** | frame-stamped event list | `Note` | e.g. a payload-less `Trigger` |

`Int` is **not** a runtime shape: a runtime integer is a `Float` you round (a modulatable
step/divisor) or an `Enum` (a bounded set). `Int` survives only as a `Constant` shape (below).

The process to add a shape:

1. **Identify the discipline** — numeric-stream / held-scalar / held-struct / sparse-event.
2. **Fits an existing discipline** → *cheap*: add a `Copy`, alloc-free type, a read accessor, a
   contract-macro keyword, and a schema entry; reuse the arena. A PR, not necessarily an ADR.
3. **Needs a new discipline** (genuinely new carry/serve semantics — its own arena, accessor,
   slicing rule) → *an ADR*, like 0015. High bar: show the four cannot serve it.

Guardrails for any shape: it must be **`Copy` and allocation-free** (it crosses the audio
thread); it must have a defined **unwired default**; it is a **specific named type, never a
generic/open struct** (an open struct is what would break the `Copy` guarantee — `Harmony` and
any future `Transport` are concrete, enumerated types); and **cross-shape use is always an
explicit converter operator**, never implicit coercion. The closed-ness is principled: it falls
out of the `Copy`/alloc-free audio-thread constraint plus the per-discipline arena cost, not from
arbitrary restriction.

### Density is the engine's job; `Float` is always a buffer underneath

For a `Float`, *dense vs held* is a performance detail the engine decides from the wired source,
never something the author declares:

- wired to a dense `Float` producer (audio, a contour) → the real buffer, passed through,
- fed by sparse messages / a literal / unwired → a scratch buffer **materialized** from the
  latched current scalar; a mid-block message change is **written into the buffer at its frame**,
  so sample-accuracy is automatic with no process re-slicing. Held-unchanged values are
  **cached** (refilled only on change), so the steady-state cost is ~nil. Scratch lives in the
  existing Signal arena ([ADR-0001](0001-unified-block-graph-execution.md)).

The buffer carries a cheap **`varying: bool`** the engine already knows (held-and-unchanged =
false; dense or changed-this-block = true).

This retires [ADR-0017](0017-playable-surface-and-control-domain.md)'s carrier doctrine —
"Message is the default control domain, Signal is the opt-in special case, cross-domain
conversion is always an explicit operator." There is no domain boundary to cross. The
interpolation question it raised does not vanish; it **splits**:

- *materialize sparse → dense* is now **automatic** (the engine). Off the author's plate.
- *step vs slew vs glide* survives as an **optional `Float → Float` shaper operator**
  (`slew`, `glide`) — the same category as `power` ([ADR-0027](0027-envelope-emits-cv-and-curve-ops.md)),
  inserted only when smoothing is wanted, never a carrier converter.

`m2s` is therefore **demoted**: its "make it a Signal" job is gone (automatic); its
`slew`/`smooth`/`glide` modes become the `Float → Float` shaper(s); its `snap` mode was always
"what block-slicing already does," now the default.

The same "conversion is explicit" principle survives, **reframed from carriers to shapes**:
crossing a *shape* boundary needs an operator. `Float → Enum` is a quantizer; `Float → Note` is a
threshold/trigger op; there is no implicit coercion between shapes.

### Two read views on a `Float`, chosen by the operator's processing model

Not every operator processes per-sample. Block-rate ops (a clock reading tempo, a control
reader) have no business looping a buffer to read one value. So a `Float` exposes **two read
views over the same underlying state** (the engine's latched scalar *is* the buffer, expanded):

- **per-sample DSP** (osc, filter, `mul`, `power`, envelope) → `io.signal(IN) -> &[f32]` + the
  `varying` hint,
- **block-rate / scalar** (clock tempo, sample-and-hold, a control reader) → `io.value(IN) ->
  f32`, reading the latched current scalar directly — no buffer materialized.

This is **not** the two-path read we are killing. That branch (`io.input().map_or(io.param(),
…)`) was conditional on **runtime wiring state**, mandatory for correctness, written in every op.
`io.signal` vs `io.value` is a **static choice intrinsic to the operator** — "am I per-sample or
block-rate?" — fixed at authoring, never conditional on what's wired. A filter always calls
`io.signal`; a clock always calls `io.value`.

`const`-folding (e.g. a filter recomputing biquad coefficients only when `cutoff` changes) is an
**optional, additive** optimization the op opts into via `varying` — a naive op ignores it, reads
`buf[i]`, and is correct.

### Outputs mirror inputs

An output is also just a `shape`, and `Float` outputs have the symmetric two write views:

- per-sample producer (osc, envelope) → `io.signal_mut(OUT) -> &mut [f32]`,
- block/event-rate producer (a control emitter) → `io.set_value(OUT, x)` (a scalar write the
  engine materializes into a held `Float` for downstream).

Worth seeing:

- `oscillator.audio`, `filter.audio`, `mul.out` = `Float` (dense buffers).
- `envelope.cv` = **`Float`** — semantically "a level," but a 2 ms attack stair-steps if
  decimated, so it is produced and consumed per-sample like any other `Float`. It wires into
  `filter.cutoff` (also `Float`); the engine serves it densely because the source is dense. The
  former "audio vs CV vs control" trichotomy is **all one shape**.
- `ctx.harmony` = `Harmony` (held struct, publish-on-change — already exactly this,
  [ADR-0015](0015-latched-context-read.md)).
- a sequencer's `notes` = `Note` (frame-stamped events).

A producer and consumer never need matching "temporality" — each is just a shape; a block-rate
`Float` producer can feed a per-sample `Float` consumer and vice-versa, engine bridging. The one
illegal wiring is a **shape mismatch** (`"audio": "Hp"`, or a `Float` wired to a `Note` input),
which replaces `PortKindMismatch`.

### `Constant` is a separate noun — instantiate-time configuration, not an `Input`

A **`Constant`** configures an operator *instance* at instantiate time and never changes on the
data path. The boundary is precise: **a value is a `Constant` iff changing it would rebuild the
graph.** The canonical case is `voices` — it sets lane count, hence buffer allocation and
topology ([ADR-0010](0010-single-lane-operators.md), `LaneRule::FromParam`), so it cannot be a
runtime value. Constants live in a `config` block, declared by name with their own shape
(`Enum`/`Int`).

**Shape does not decide `Constant`-vs-`Input`.** `mode` (LP/HP/BP) and `waveform` (sine/saw) are
`Enum`s, but changing them rebuilds nothing — only which coefficients are computed — so they are
**runtime `Enum` inputs**, switchable live over OSC (reuben is a *playable* system). Only
genuinely topology-fixing values are `Constant`s; the carve-out stays small.

### "Audio vs control" is tooling metadata, not a type

Collapsing audio, CV, and control into one `Float` shape drops one thing the old Signal/param
split implied: the authoring *intent* "this is an audio/CV cable" vs "this is a control knob,"
which the control-surface generator ([ADR-0018](0018-control-surface-generation.md)) and patcher
([ADR-0020](0020-introspection-and-patcher-skill.md)) care about. A modular synth distinguishes a
knob from a CV jack though they sum identically. That intent survives as **optional tooling
metadata** alongside `control`, **never** as a runtime type — the engine treats all `Float`s
alike.

### The patch: one `inputs` map, one `config` block

Value-sources stop being split across `params` (literals) and `connections` (wires) by accident
of carrier. One `inputs` map; a value is a **literal** *or* a **wire-ref**; Constants live in
`config`. The top-level `connections` array is removed.

```json
{
  "type": "filter", "address": "/filt",
  "config": { "mode": "Hp" },
  "inputs": {
    "audio":     { "from": "/osc.audio" },
    "cutoff":    { "from": "/lfo.audio" },
    "resonance": 0.4
  }
}
```

`cutoff: 1000` and `cutoff: { "from": "/lfo.audio" }` target the **same slot**; `mode` is a named
enum, not `0`/`1`/`2`.

### The contract macro declares it once

[ADR-0025](0025-single-source-operator-contract.md)'s single-source `operator_contract!` stays
single-source: `inputs`/`outputs` carry a `shape` with the default folded in; structural config
moves to `config`.

```rust
operator_contract!(Filter {
    inputs:  { audio: float, cutoff: float { 20..=20_000, default 1000, "Hz", exp },
               resonance: float { 0..=1, default 0.2 } },
    outputs: { audio: float },
    config:  { mode: enum { Lp, Hp, Bp } },
});
// process — one read per input, chosen by processing model, never by wiring:
let audio = io.signal(IN_AUDIO);
let cut   = io.signal(IN_CUTOFF);          // buffer view; `varying` lets it const-fold coeffs
let mode  = io.enum(IN_MODE);              // held scalar
for i in 0..n { /* one buffer loop */ }
```

`cutoff`/`resonance`/`freq` are now declared **once**, not as a port *and* a param.

## Consequences

- **Breaking, engine-wide.** `PortKind` is deleted; `Descriptor` carries `Input`/`Output` (a
  `shape`) and `Constant`. `Io` exposes `signal()`/`value()`/`enum()`/`harmony()`/`events()` and
  `signal_mut()`/`set_value()` in place of `input()`/`param()`/`context()`. Every operator is
  migrated; the golden descriptor snapshot and generated instrument schema are re-blessed; all
  bundled instruments move to `inputs`/`config`.
- **The conditional read disappears.** `io.input(..).map_or(io.param(..), ..)` becomes one
  `io.signal(..)` or `io.value(..)`. `cutoff`/`resonance`/`freq` are declared once each.
- **`Float` is always a buffer underneath; materialize replaces block-slicing for `Float`.** The
  engine writes mid-block changes into the buffer (sample-accurate, one `process()` call).
  Block-slicing **survives only** for the scalar/event shapes — `Enum`, `Harmony`, `Note` — whose
  reads need a sub-block boundary.
- **`m2s` demoted.** The carrier bridge is removed (`snap` is now the engine default);
  `slew`/`glide` survive as `Float → Float` shaper ops. Instruments rewire accordingly.
- **`mode`/`waveform` become live-switchable** (`Enum` inputs) — a capability gain over the old
  `param`. `voices`-like topology values become explicit `Constant`s.
- **Terminology retired:** **"Context" → `Harmony`** (a shape); **"Signal," "carrier,"
  "Level/Stream"** are no longer types — `io.signal()` names only the *buffer read-view* of a
  `Float`. **CV / audio / control** are one shape (`Float`) plus optional tooling metadata.
- **Supersedes** the carrier portions of [ADR-0017](0017-playable-surface-and-control-domain.md):
  "Message is the default control domain," "two carriers, one value," the **one-port-one-type
  rule** (subsumed — there is now literally one Input per function), and the M→S converter
  doctrine. **Retained from 0017:** the math-operator family
  (`add`/`mul`/`map`/`differentiate`/`integrate`) and **Good Button** composition — orthogonal,
  now operating on `Float`s. (`differentiate`/`integrate` shift from event-rate to per-sample
  calculus; an event-rate variant, if wanted, is an explicit construction — decided in the sweep.)
- **Amends** [ADR-0011](0011-message-delivery-and-timing.md) (block-slicing now serves only the
  scalar/event shapes; `Float` param updates are materialized into the buffer) and
  [ADR-0015](0015-latched-context-read.md) (Context becomes the `Harmony` shape; the latch +
  `Copy` resolver struct survive; the dedicated context arena/accessor folds into shape delivery).
- **Reinforces** [ADR-0001](0001-unified-block-graph-execution.md) ("CV and audio are the same
  Signal" generalizes to "density is not a concept; there is just `Float`") and
  [ADR-0027](0027-envelope-emits-cv-and-curve-ops.md) (composition over coupling: a `slew` shaper
  is the same move as `power`).
- **Authoring contract:** `authoring.md` and the create-operator skill
  ([ADR-0021](0021-scaffold-operator-and-create-operator-skill.md)) updated — operators are
  authored as `Input { shape, default }` + `Constant`, with the legal cross-shape conversions
  named and the per-sample/block-rate read views explained.
- **Deferred:** the migration is large; sequencing (engine core → macro → operator sweep →
  instruments → docs/schema) is its own implementation thread, not settled here.
