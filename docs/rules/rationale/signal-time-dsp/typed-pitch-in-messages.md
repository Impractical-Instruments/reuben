# Why: In the Message domain pitch is explicitly typed — the `Pitch` enum's own case distinguishes an absolute MIDI coordinate from a scale degree, so no operator has to guess what a number means — while the Signal domain stays untyped.

[Rule](../../signal-time-dsp.md#typed-pitch-in-messages)

Designing the tonal-context note path forced a decision the pitch model left implicit: whether
pitch-like numbers are typed or "just numbers." The two regimes already in the engine want **opposite**
answers. The **Signal domain stays numeric** — a freq Signal is Hz-as-`f32`, CV is `f32`, untyped and
fungible so audio-rate FM or a pitch envelope into a cutoff just works; the useful weirdness lives here
and must stay untyped. The **Message domain is explicitly typed**, because discrete musical events are
exactly where "just a number" causes silent misreads (is `64` a MIDI note, a scale degree, or Hz?).

The typing is **minimal and carried by the value**: `Pitch` is one enum, `Degree(i32)` or
`Absolute(f32)`, so a downstream op branches on the case rather than on a port name. A
type-discriminated *arg* (`Int`=degree, `Float`=MIDI) was rejected because it **collides at the
OSC boundary**: a MIDI keyboard sends note numbers as ints meaning *absolute*, which a type tag would
silently reinterpret as degrees. Converters are explicit, context-aware operators — a `snap`/quantize
op (absolute→degree) and the `pitch2freq` lowering (degree→Hz) — which is what makes "diatonic vs
chromatic transpose" expressible at all: the transpose op's behavior is *defined by* the pitch type it
receives (a degree shifts by whole steps, an absolute MIDI pitch by semitones), an ambiguity raw
numbers cannot carry. The concrete payoff of the determinism invariant: the sequencer's default degree
pattern `[0..7]` under default C-major/12-TET is **bit-identical** to the old MIDI default
`[60,62,64,65,67,69,71,72]`, but now re-spells live on a key change. Promotion to a first-class arg
type (adding `Interval`, pitch-class, …) waits until an operator must carry mixed pitch kinds on one
port.

Distilled from: ADR-0008
