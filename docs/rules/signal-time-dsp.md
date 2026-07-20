# Signal, OSC, musical time & DSP

> How signal and musical meaning are carried, timed, and shaped — the OSC-only Message model, the Clock and musical time, symbolic pitch and Tuning, the tonal-context bus, and the envelope/curve/math DSP families.

## Now

The core speaks exactly one language: the **OSC-shaped Message** — an address, a timestamp, and a
typed payload (its `Message`/`Arg` substrate is the [composition-operators](composition-operators.md)
topic's to define). Every foreign protocol — MIDI, Ableton Link, OSC tempo sync from a foreign
clock — is converted to and from that shape by an isolated, removable **boundary adapter**, so no
operator ever branches on protocol and each adapter detaches with the native layer. Addresses are
hierarchical: every operator, port, and param is auto-addressable by its **structural path** through
the graph nesting (`/lead-synth/filter/cutoff`), and an instrument additionally publishes a curated
set of **exposed** named addresses — its public control surface — that survive internal rewiring.
OSC wildcard/pattern dispatch (`/drums/*/decay`) is the *designed* meta-effect mechanism but is **not
yet implemented as internal routing** — internal edges are statically wired and port-bound today, and
wildcards remain a boundary-layer intention (see the [execution-runtime](execution-runtime.md) topic's
operator-message-emission rule for why internal routing is addressless).

Musical time is a **hybrid Clock**: one default Clock makes any two Toys dropped in a Rig groove
together out of the box, but a Clock is itself an Operator (a sample-accurate beat phasor), so
polytempo, clock division, and independent timing are patched when wanted. The Clock provides *base
timing only* — tempo, meter, the beat grid; **groove, swing, and feel are separate Operators** that
re-time Message streams per-stream. Message timetags default to musical time and resolve to a sample
offset against the active Clock at dispatch, so a tempo change re-times everything for free; absolute
sample-time tags stay available for transport-independent events.

Pitch is **two layers**: a symbolic degree within the active Scale (with float MIDI as an always-
available 12-TET coordinate) that a **Tuning** resolves to Hz — 12-TET is merely the default Tuning,
and Scala `.scl`/`.kbm` is the import format for the whole microtonal world. In the Message domain
pitch is *explicitly typed* (an absolute pitch versus a scale degree, distinguished by port/address
role and type-checked at load); the Signal domain stays untyped so audio-rate weirdness patches
freely. The current key/scale/chord/tuning is the **tonal context**, and — exactly like the Clock, and
for the same polytonality reason — it is an **Operator**, not a global: a default context node grooves
a Rig into one key, multiple nodes give polytonality. The context node owns the resolver (`hz`,
`snap`, `chord_tone`) as a deep module, so followers stay dumb and read `io.context().hz(p)`; its
value is a small `Copy` struct of optional fields written per-field last-write-wins. Followers read
that value through the engine's per-port **latch** and its changes are sliced sample-accurately — both
mechanisms owned by the [execution-runtime](execution-runtime.md) topic (its latch-service and
sample-accurate-timing rules); this topic only fixes *what* rides that wire and *who* resolves it.

DSP is authored as small, composable operators. An **envelope** is a pure generator emitting a linear
CV contour in `[0, 1]`; downstream ops decide what it means, and the VCA is an explicit `mul` — so the
same contour drives amplitude, pitch, or filter motion. **Curve ops** are named for their exact math
(`power` = `x^exponent`), never a generic "curve" knob. The **math family** is dense `Float`→`Float`
ops, one operator per module, with the old shared `Number`-trait core retired; the calculus ops
(`differentiate`, `integrate`) are dense with a constant one-sample `dt`, which is what keeps
higher-order calculus valid.

## Rules

<a id="osc-only-core"></a>
### The core speaks only OSC-shaped Messages; every other protocol is converted to and from OSC by isolated, removable boundary adapters.

[why](rationale/signal-time-dsp/osc-only-core.md)

<a id="structural-and-exposed-addressing"></a>
### Every operator, port, and param is auto-addressable by its structural path through the graph, and an instrument additionally exposes a curated set of stable named addresses as its refactor-safe control surface.

[why](rationale/signal-time-dsp/structural-and-exposed-addressing.md)

<a id="clock-is-an-operator"></a>
### A single default Clock grooves every Toy together out of the box, but Clocks are Operators, so polytempo, clock division, and independent timing are patched when wanted.

[why](rationale/signal-time-dsp/clock-is-an-operator.md)

<a id="groove-is-separate-operators"></a>
### Groove, swing, and feel are separate Operators that re-time Message streams per-stream, not behavior buried in the Clock.

[why](rationale/signal-time-dsp/groove-is-separate-operators.md)

<a id="musical-timetags"></a>
### Message timetags default to musical time, resolved to a sample offset against the active Clock at dispatch, with absolute sample-time tags available for transport-independent events.

[why](rationale/signal-time-dsp/musical-timetags.md)

<a id="two-layer-pitch"></a>
### Pitch is a two-layer model — a symbolic degree within the active Scale (with float MIDI available as a 12-TET coordinate) resolved to Hz by a Tuning — and 12-TET is just the default Tuning.

[why](rationale/signal-time-dsp/two-layer-pitch.md)

<a id="scala-tuning-import"></a>
### Tunings are defined and interchanged as Scala .scl/.kbm, so the existing microtonal world imports and users can define their own.

[why](rationale/signal-time-dsp/scala-tuning-import.md)

<a id="typed-pitch-in-messages"></a>
### In the Message domain pitch is explicitly typed — an absolute pitch versus a scale degree, distinguished by port and address role and type-checked at load — while the Signal domain stays untyped.

[why](rationale/signal-time-dsp/typed-pitch-in-messages.md)

<a id="tonal-context-is-an-operator"></a>
### Tonal context (tuning, root, scale, chord) is an Operator like the Clock — a default instance grooves a Rig into one key and multiple context nodes give polytonality — never a global.

[why](rationale/signal-time-dsp/tonal-context-is-an-operator.md)

<a id="context-owns-resolution"></a>
### The tonal-context node owns pitch resolution and snap as a deep module — degree to step via the symbolic Scale, step to Hz via the Tuning — so followers read Hz through io.context() rather than composing the chain themselves.

[why](rationale/signal-time-dsp/context-owns-resolution.md)

<a id="context-per-field-lww"></a>
### Tonal context is one bundled struct of optional fields written per-field last-write-wins, and genuinely different scopes or lifetimes split into separate context nodes.

[why](rationale/signal-time-dsp/context-per-field-lww.md)

<a id="pitch-snap"></a>
### Snap quantizes an arbitrary pitch to the nearest in-scale degree — tuning-correct distance, deterministic down tie-break, policy supplied per call — returning a symbolic degree that re-resolves if the tuning swaps.

[why](rationale/signal-time-dsp/pitch-snap.md)

<a id="envelope-emits-cv"></a>
### The envelope is a pure generator emitting a linear CV contour in [0, 1]; downstream ops interpret it, and the VCA is an explicit mul rather than a baked-in behavior.

[why](rationale/signal-time-dsp/envelope-emits-cv.md)

<a id="curve-ops-named-for-math"></a>
### Curve and shaping ops are named for their precise math — power is x^exponent — each its own operator rather than a generic curve knob with a mode param.

[why](rationale/signal-time-dsp/curve-ops-named-for-math.md)

<a id="math-family-dense-float"></a>
### Every math op is a dense Float-to-Float operator authored as one operator per module, and the shared Number-trait core is retired.

[why](rationale/signal-time-dsp/math-family-dense-float.md)

<a id="calculus-constant-dt"></a>
### differentiate and integrate are dense Float-to-Float ops with a constant one-sample dt, which is what keeps higher-order calculus valid.

[why](rationale/signal-time-dsp/calculus-constant-dt.md)

## Terms

<!-- Each term this topic defines. Collated into the rules index glossary. One per topic. -->
- **Boundary adapter** — a removable I/O-edge component that converts a foreign protocol (MIDI, Ableton Link, external OSC) to and from the core's OSC-shaped Messages.
- **Clock** — the Operator providing base musical timing — tempo, meter, the beat grid — as a sample-accurate beat phasor; a default instance syncs a Rig.
- **Groove** — a per-stream re-timing of a Message stream (swing/feel), applied by a separate Operator, distinct from the Clock's base grid.
- **Tuning** — the resolution layer mapping a symbolic pitch (a scale step) to a frequency in Hz; 12-TET is the default, Scala-importable.
- **Scale** — ordered step-offsets within a Tuning's period plus a root, mapping a scale degree to a step index (symbolic → symbolic).
- **Tonal context** — the latched key/scale/chord/tuning value, owned by a context Operator, that followers resolve pitch against.
- **Snap** — quantizing an arbitrary pitch to the nearest in-scale degree under a caller-supplied policy, upstream of resolution.
- **CV** — a linear control signal in a normalized range (e.g. an envelope's `[0, 1]` contour), carried untyped on the Signal domain and interpreted by downstream ops.
