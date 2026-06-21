# Pitch and tuning: symbolic pitch + Scala-based resolvable Tuning

## Context

reuben must fully support non-Western tonalities and user-definable tunings, not just 12-TET. Research into current practice (2024–2026) found a consistent two-job split: **Scala `.scl`/`.kbm`** is the decades-old de facto standard for *defining* scales/tunings (cents/ratios/EDO-steps; covers gamelan, maqam, Carnatic, just intonation, non-octave scales; 5000+ archive; adopted even by Ableton Live 12). **MTS-ESP** (ODDSound, ~2021, 0BSD license) is the modern standard for *dynamic real-time* retuning — clients query per-note frequency continuously during playback, not just at note-on. MIDI 2.0/MPE provide per-note pitch transport but are not scale-definition formats. Every serious engine separates symbolic pitch identity from frequency resolution; 12-TET is never baked in.

Sources: Scala format (huygens-fokker.org/scala/scl_format.html), MTS-ESP (github.com/ODDSound/MTS-ESP), Surge tuning-library (github.com/surge-synthesizer/tuning-library), Ableton ASCL spec, Scale Workshop/SonicWeave (github.com/xenharmonic-devs).

## Decision

- **Two-layer pitch model.** A **symbolic** layer — scale degree within the active Scale (primary), with float MIDI note (60.0 = middle C) available as a 12-TET coordinate — and a **resolution** layer where a **Tuning** maps symbolic pitch to frequency (Hz), which is what oscillators consume. 12-TET is just the default Tuning.
- **Scala `.scl`/`.kbm` is the import/interchange format** for Tunings (parsed in Rust, or via Surge's header-only `tuning-library`). Instant access to the existing microtonal world plus "define your own."
- **The active Tuning rides the tonal-context bus** (alongside key/scale/chord) and is queried continuously — adopting MTS-ESP's *model*. Dynamic retuning while notes sound falls out for free; operators ask "what Hz is degree 3 right now."
- **MTS-ESP / MPE / MIDI 2.0 pitch are boundary adapters** (ADR-0007), not core concepts, so the ecosystem is reachable later (e.g. CLAP hosting) without polluting the core.

## Considered and rejected

- **Integer-MIDI / 12-TET baked into the core:** kills microtonality and non-Western tonalities.
- **Frequency (Hz) as the symbolic layer:** fully general but loses musical meaning and snap-to-scale.
- **Float-MIDI-note as the primary symbolic layer:** kept as available, but scale-degree-primary gives free transposition and the "good button" snap-to-scale.

## Consequences

- Need a Scala parser in Rust (small) or an FFI to `tuning-library`.
- The tonal-context bus must carry the active Tuning, not just key/scale/chord.
- SonicWeave `.swi` import is a possible later addition; Scala covers v1.

## Amendment (2026-06): explicit, minimal pitch types

Designing the tonal-context note path ([ADR-0013](0013-tonal-context-bus-mechanics.md),
[ADR-0015](0015-latched-context-read.md)) forced a decision this ADR left implicit: whether
pitch-like numbers are typed or "just numbers." Resolution — the two regimes already present in
the engine want **opposite** answers:

- **Signal domain stays numeric.** A freq Signal is Hz-as-f32; CV is f32. Untyped, fungible,
  patch-anything-into-anything (audio-rate FM, a pitch envelope into a cutoff). The useful
  "weird" lives here; keep it untyped.
- **Message domain is explicitly typed.** Discrete musical events are where "just a number"
  causes silent misreads (MIDI vs degree vs Hz). Type them.

**Decision:** in the Message domain, pitch is **explicitly typed**, starting **minimal** — two
kinds: `AbsolutePitch` (MIDI/Hz-bound) and `ScaleDegree` (context-relative). The distinction is
expressed via **port/address role** first (`note` = absolute, `degree` = symbolic), reusing the
existing Instantiate-time type-check (`PortKindMismatch`) so a degree source into an absolute
input is a **load error**. This is preferred over type-discriminated args (`Int`=degree,
`Float`=MIDI) because the latter **collides at the OSC boundary** — a MIDI keyboard sends note
numbers as ints meaning *absolute*, which a type tag would silently reinterpret as degrees.

**Converters are explicit operators**, each context-aware and well-defined: a `quantize` op
(absolute→degree, reads context) and the resolver in the Voicer (degree→Hz, reads context).
This makes "diatonic vs chromatic transpose" expressible — an ambiguity raw numbers cannot
carry: the transpose op's behavior is defined by the pitch type it receives.

**Grow on demand.** Promote to a first-class `Arg`/value type (and add kinds like `Interval`,
pitch-class) only when an operator must carry mixed pitch kinds on one port; until then,
port-role typing is lighter and keeps the OSC boundary simple. Signals remain untyped — cross-
domain weirdness is an *explicit* convert to CV, then patch the result anywhere.

Consequence: the sequencer emits `ScaleDegree` (`degree` port). Its default pattern `[0..7]`
under the default C-major/12-TET context is **bit-identical** to the prior MIDI default
`[60,62,64,65,67,69,71,72]` (determinism invariant, ADR-0001), but now re-spells live on a
key/scale change.
