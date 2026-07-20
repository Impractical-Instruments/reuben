# Why: In the Message domain pitch is explicitly typed — an absolute pitch versus a scale degree, distinguished by port and address role and type-checked at load — while the Signal domain stays untyped.

[Rule](../../signal-time-dsp.md#typed-pitch-in-messages)

Designing the tonal-context note path forced a decision the pitch model left implicit: whether
pitch-like numbers are typed or "just numbers." The two regimes already in the engine want **opposite**
answers. The **Signal domain stays numeric** — a freq Signal is Hz-as-`f32`, CV is `f32`, untyped and
fungible so audio-rate FM or a pitch envelope into a cutoff just works; the useful weirdness lives here
and must stay untyped. The **Message domain is explicitly typed**, because discrete musical events are
exactly where "just a number" causes silent misreads (is `64` a MIDI note, a scale degree, or Hz?).

The typing starts **minimal** — two kinds, an absolute pitch (MIDI/Hz-bound) and a scale degree
(context-relative) — and the distinction is carried by **port/address role first** (`note` = absolute,
`degree` = symbolic), reusing the existing Instantiate-time type-check so a degree source wired into an
absolute input is a **load error**, not a render-time surprise. Port-role typing is preferred over a
type-discriminated arg (`Int`=degree, `Float`=MIDI) specifically because the latter **collides at the
OSC boundary**: a MIDI keyboard sends note numbers as ints meaning *absolute*, which a type tag would
silently reinterpret as degrees. Converters are explicit, context-aware operators — a `snap`/quantize
op (absolute→degree) and the resolver in the Voicer (degree→Hz) — which is what makes "diatonic vs
chromatic transpose" expressible at all: the transpose op's behavior is *defined by* the pitch type it
receives (a degree shifts by whole steps, an absolute MIDI pitch by semitones), an ambiguity raw
numbers cannot carry. The concrete payoff of the determinism invariant: the sequencer's default degree
pattern `[0..7]` under default C-major/12-TET is **bit-identical** to the old MIDI default
`[60,62,64,65,67,69,71,72]`, but now re-spells live on a key change. Promotion to a first-class arg
type (adding `Interval`, pitch-class, …) waits until an operator must carry mixed pitch kinds on one
port.

Distilled from: ADR-0008
