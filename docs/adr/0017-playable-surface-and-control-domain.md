# The playable surface: Message-first control, one-port-one-type, and Good Buttons from composition

> **Superseded in part by [ADR-0028](0028-one-input-shape.md).** The carrier doctrine here —
> "Message is the default control domain, Signal the opt-in special case," "two carriers, one
> value," the **one-port-one-type rule**, and the explicit Message→Signal (`m2s`) converter — is
> retired: there is now one `Input` per function described by a `shape`, a `Float` is always
> materialized to a buffer (so sparse→dense is automatic), and the only illegal wiring is a
> *shape* mismatch. **Retained:** the math-operator family (`add`/`mul`/`map`/`differentiate`/
> `integrate`) and **Good Button** composition — now operating on `Float`s.

## Context

The [V1.2 roadmap](../../ROADMAP.md) names two deliverables — *performance-input mapping*
(how gestures map to Messages) and a *curated control surface* (an Instrument's public set of
good controls). The grilling collapsed both into **one concept** — the Instrument's public
*boundary* (gestures/values in, music/audio out) — and then, by following the dependencies
down, discovered that the V1.2 *build* is almost entirely **new operators**, not new
instrument-format machinery. This ADR records the control-domain model the surface rests on,
the operator-authoring rule it forces, and the small operator set that delivers it; it also
records what was deliberately pushed to the (still-ungrilled) **nesting / contract** thread.

Three existing facts framed the tree:

- **Control reaches operators only as Messages today.** An external OSC datagram becomes a
  block-quantized `Message` ([`osc.rs`](../../crates/reuben-native/src/osc.rs),
  [ADR-0007](0007-osc-only-core.md)) that either sets a node param (a block-sliced `f32`
  scalar, [ADR-0011](0011-message-delivery-and-timing.md)) or is delivered as an event. There
  is **no path from an external control to a Signal**.
- **Signals are the audio-rate carrier; only some inputs accept them.** "CV and audio are the
  same Signal" ([ADR-0001](0001-unified-block-graph-execution.md)), but a Signal can only
  drive an input the operator *exposes as a Signal port* — today just the oscillator's `freq`.
  The filter's cutoff/resonance are params, unmodulatable.
- **Message outputs already fan out.** `Plan.msg_targets` is `Vec<Vec<usize>>` — one Message
  output port routes to *many* downstream nodes ([`plan.rs`](../../crates/reuben-core/src/plan.rs)).

This ADR settles: which domain control flows in, how the two domains interact and convert,
the rule for how an operator exposes any single function, the math/conversion operators V1.2
adds, and how a "Good Button" is built — from composition, with no format change.

## Decision

### Message is the default control domain; Signal/CV is the opt-in special case

Continuous control flows as **Messages** by default — cheaper, and it sidesteps the
interpolation question (below) until you actually want audio-rate. **Signal/CV is the special
case** you reach for when audio-rate modulation is musically real (FM, filter sweeps,
vibrato). Triggers and notes are, as today, Messages.

Rejected — *Signal-first control*: it forces every modulatable param to grow a CV port and
forces an interpolation policy on every value the instant it enters the graph, which is fake
precision for slow human gestures.

### Two carriers, one value; param and CV port are the two read-views

A value has two representations — **Message** (sparse, timetagged, event-driven, sub-audio
rate) and **Signal** (dense, per-sample, audio-rate) — the same number on a different carrier.
An operator consumes a value through exactly one of two read-views: a **param** (the Message
domain's "current scalar," block-sliced) or a **Signal input port**. No reserved noun is
minted for the value itself; if one is ever needed, **`Scalar`** / **`Number`** is the
candidate (it would name the trait bound of the generic math family, below).

**Cross-domain conversion is always explicit — an operator, never implicit coercion** —
because each direction needs an *authored policy*. Wiring a Message port to a Signal port is a
**type error** (`PortKindMismatch` already enforces this), resolved by inserting a converter.

### The one-port-one-type rule (standing operator-authoring rule)

> A functional input is **exactly one port of one type**. Never duplicate a param *and* a CV
> port for the same quantity. Favor a **Signal input** where audio-rate modulation is musical
> (freq, cutoff, amp, pan); use a **Message param** for discrete/structural controls
> (waveform, mode, voice count, room size). A Signal input port carries an **unwired default
> scalar** (so static use needs no converter — the default is the one scalar that survives
> from the old "param"). To drive a Signal input from Messages, insert the explicit M→S
> converter — interpolation logic lives **once** in the converter, never re-implemented per
> node. In doubt, favor the higher-resolution (Signal) input.

This kills the "param **and** CV port for the same function" duplication at the root — and
with it the question of how a param and a CV value *combine* at a port. There is no
combination: base-plus-modulation is built **explicitly** with an `add` operator in the
relevant domain, feeding the single port.

Consequence — a **full sweep of the existing operators now** (bounded; done once, no piecemeal
migration later): each functional input reclassified to one port of one type, Signal ports
given defaults. Concretely: **oscillator `freq` → Signal-only** (the `freq` param survives
only as the port's unwired default); **filter `cutoff`/`resonance` → Signal inputs** (canonical
sweep targets). Params persist where audio-rate is meaningless.

This rule is added to [authoring.md](../agents/authoring.md) and belongs in the future
"create operator" skill ([V1.6](../../ROADMAP.md)).

### A generic math-operator family: one core, generated per-domain/per-type shells

The mapping richness comes from a family of small math operators (`map`, `add`, `mul`,
`differentiate`, `integrate`, …). Each op's arithmetic is written **once** against a `Number`
trait; a macro generates the thin descriptor shells — the **Message variants per numeric type**
and the **Signal variant over an f32 buffer**. The "mirror" between domains *is* the shared
core, so the two domains cannot drift. Adding a math op = write the core + one macro line.

An asymmetry is structural and accepted: **Signal is f32-only** (audio buffers are float),
**Messages are multi-type** (`Float | Int | Bool | Sym`). So int math exists only in the
Message domain; the Signal mirror is f32 alone.

*Implementation note (V1.2 as built).* A second asymmetry surfaced from the message-routing
model and is accepted: a delivered `Event` carries only its node-local **address**, not the
destination **input port** it arrived on, so a *multi-input* Message operator can't tell its
operands apart. The V1.2 family is therefore split by arity, not mirrored one-for-one:
**binary ops (`add`, `mul`) are Signal-domain** (two buffers — and combining two *streams* is
modulation, which is where the Signal domain naturally lives), while the **Message domain gets
the single-input ops** (`map`, `differentiate`, `integrate`), which consume any incoming value
event and so compose freely. Multi-input Message arithmetic (and the per-int-type Message
shells) waits on **port-tagged Message routing** — a small, isolated deferral, noted below.

- **Pointwise ops** (`map`, `add`, `mul`) need no time.
- **Calculus ops** (`differentiate`, `integrate`) need `dt`. In the Message domain `dt` is
  **real time from the sample-accurate `frame`** every Message already carries — `velocity =
  Δvalue / Δt_sec`, `integrate: acc += value · Δt` — accumulated across block boundaries with a
  little state. Internal message chains get sample-accurate `dt`; external messages are
  block-quantized (already accepted as honest). Emission is event-driven: one output Message per
  input Message, stamped with the triggering frame. (Per-message-count `dt` rejected — it makes
  "velocity" meaningless for irregular streams.)

`map` is **1:1** (one input → one transformed output). A "fan map" (one input, N
differently-ranged outputs) was rejected: it buys nothing over Message fan-out, and a
variable-output-arity operator has no home in the static-`Vec` descriptor model.

### The Message→Signal converter: one operator, a `mode` param

The single sanctioned M→S bridge is one operator with a **`mode`** param — the authored
"how do I fill the gaps" choice the explicit-conversion rule demands:

- **snap** — step at the message frame (sample-accurate; what param block-slicing already does,
  materialized as a Signal).
- **slew** — rate-limited approach (`rate`).
- **smooth** — one-pole exponential approach (`time`); the natural knob feel.
- **glide** — fixed-time linear ramp (`time`); portamento, retargeting per message.

True linear-interpolation-*between-messages* is **excluded** — it needs the next message, so
it is not RT-causal without a one-block delay. Bundling the modes into one operator (rather
than four) is the one principled exception to "one op, one thing": the *thing* is "bridge to
CV," and mode is its character; it also keeps the M→S decision a single authored knob.

**Signal→Message conversion is deferred** — sampling a Signal back to discrete values needs a
*sampling* policy (on a trigger/clock, or decimation); that is envelope-follower / sampler
machinery, pulled in when a Toy needs it.

### A "Good Button" is built from composition — no format change

**Good Button** is the official term — for **both** the design principle (a control that is
hard to make sound bad) and the **artifact** (a curated, often mapped, control). "Meta param,"
"meta-control," "macro" all name the same artifact and are **avoided**.

A Good Button is assembled from the operators above plus *existing* wiring, so **V1.2 needs no
instrument-format change**:

- The fan is free: an identity `map` (`[0,1]→[0,1]`) at the public address, its Message output
  fanned via existing `msg_targets` to N ranged `map`s — e.g. `map_cutoff [0,1]→[800,10000]`,
  `map_res [0,1]→[0.2,0.7]` — each emitting to its internal Signal/Message input. The per-target
  ranges live in the maps' `params`.
- The public address is just that node's address; control reaches it exactly as today (OSC to a
  node address). Wildcard ("`/drums/*/cutoff`" — one *pattern*, many *nodes*, same value) is a
  **separate, complementary** feature, still deferred — a Good Button is one *input* to N
  *enumerated, transformed* targets, which needs none of it.

So **V1.2's structural payload is operators, not format**: the math family + shell macro, the
M→S converter, and the one-port-one-type operator sweep.

### Deferred to the nesting / contract thread

These were agreed as *principles* but their *implementation* only earns its keep when an
Instrument is reused **as an operator** (nesting), which is itself ungrilled and unbuilt — so
they leave V1.2:

- **Surface → synthesized `Descriptor`** (an Instrument's exposed ports/params *becoming* its
  operator face, with metadata inherited from the internal param and overridable per field).
  Until nesting, a Good Button's metadata just lives on its `map` params.
- **A stable node `id`** distinct from `address` (address = identity *and* routing today). It
  buys refactor-safe public bindings, but its only consumers were a curated-surface section
  (not built) and the rename tool (below) — no V1.2 consumer. When added: `id` optional,
  **defaults to address** (zero forced migration; connections stay address-keyed), refactor
  safety **opt-in** by setting an explicit stable id, with its own uniqueness check.
- **Additive vs encapsulating surface** — when built, the surface is **additive** (curated
  addresses are aliases; structural addresses still resolve — the runtime is a flat inlined
  graph, so they exist anyway). Encapsulation rides in with nesting (a child's internals
  namespaced under the parent).
- **The address-rename refactor tool** — sweeps internal address refs within an *instrument*
  (JSON-structural, segment-aware, not text). Guard against: cross-instrument address reuse
  (scope to one document), substring/prefix bleed, node-prefix-vs-suffix, doc prose (flag for
  human review, don't auto-rewrite), external senders (can't reach — *warn*), and the
  **id-default trap** (renaming a node with no explicit id silently changes its id → **auto-pin
  `id` = old address before renaming**).

## Consequences

- **New operators (the V1.2 build, as shipped):**
  - A **math family** from a single `Number`-generic core (`operators/math.rs`): a
    `signal_pointwise!` macro stamps the Signal binary ops **`add`** / **`mul`** (each input's
    unwired default is the op's identity, so wiring one side passes the other through);
    Message-domain **`map`** (1:1 affine remap with input/output ranges + linear/exponential
    curve — the Good Button workhorse), **`differentiate`** and **`integrate`** (frame-based
    `dt`, accumulated across blocks). All registered in `Registry::builtin()`; schema
    regenerated. (Binary Message ops + per-int-type shells deferred to port-tagged routing —
    see the math-family note above.)
  - One **M→S converter** (`operators/m2s.rs`): Message in → Signal out, `mode` ∈ {snap, slew,
    smooth, glide} + `rate`/`time` + a `default` (resting value before the first message). The
    one sanctioned Message→Signal bridge. (S→M deferred.)
  - Worked examples: **`instruments/good-button.json`** (a brightness Good Button — `map` fan
    → `m2s` → filter cutoff/resonance) and **`instruments/auto-filter.json`** (base + LFO via
    Signal `add`). Both covered by integration tests and the `rt_safe` allocation check. (A
    human OSC walkthrough, `v1.2-playable-surface-testing.md`, has since been removed — it
    described this ADR's retired carrier model; see
    [v1.4-control-surface-testing.md](../v1.4-control-surface-testing.md) for the current
    walkthrough.)
- **Operator sweep (one-port-one-type):** every existing operator reclassified to one port per
  function; Signal input ports gain unwired default scalars. **Oscillator `freq` → Signal-only**
  (param becomes the port default); **filter `cutoff`/`resonance` → Signal inputs**. Update each
  operator's tests and regenerate the schema.
- **No instrument-format change in V1.2.** No new JSON section, no node `id`, no surface
  declaration. Good Buttons are composed from operators + existing connections + existing
  Message fan-out.
- **Authoring contract:** the **one-port-one-type rule** added to
  [authoring.md](../agents/authoring.md); destined for the V1.6 "create operator" skill.
- **Terminology:** **Good Button** (principle *and* artifact) — added to
  [CONTEXT.md](../../CONTEXT.md); avoid "meta param / meta-control / macro." A converter is the
  only sanctioned **M→S** bridge; **CV** = a Signal used as control (no new type).
- **Deferred (nesting / contract thread, and elsewhere):** surface → `Descriptor` synthesis;
  stable node `id` + opt-in refactor safety; additive-then-encapsulating surface; the
  address-rename tool (with id auto-pin); Signal→Message conversion; wildcard dispatch
  ([ADR-0005](0005-osc-namespace-and-wildcards.md)); a reserved `Scalar`/`Number` noun.
  **Port-tagged Message routing** — tag a delivered `Event` with the destination input port
  (not just the node-local address) so a *multi-input* Message operator can tell its operands
  apart; unlocks binary Message arithmetic (`add`/`mul` per numeric type) without the
  Signal-domain detour. Small and isolated to the routing layer.
