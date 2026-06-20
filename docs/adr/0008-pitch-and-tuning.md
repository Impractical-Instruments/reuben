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
