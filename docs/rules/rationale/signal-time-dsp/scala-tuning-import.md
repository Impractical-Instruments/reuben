# Why: Tunings are defined and interchanged as Scala .scl/.kbm, so the existing microtonal world imports and users can define their own.

[Rule](../../signal-time-dsp.md#scala-tuning-import)

Given the two-layer model ([two-layer-pitch](two-layer-pitch.md)) needs a way to *define* Tunings,
the question is which format. Scala `.scl`/`.kbm` is the decades-old de facto standard for defining
scales and tunings — cents, ratios, or EDO-steps; it covers gamelan, maqam, Carnatic, just intonation,
and non-octave scales, ships a 5000+ entry archive, and has been adopted even by mainstream DAWs. Its
whole ecosystem imports the day the parser lands, and "define your own" comes with it — a huge reach
for a small cost.

The cost is genuinely small: a Scala parser is a modest chunk of Rust, or an FFI to an existing
header-only tuning library. `.scl`/`.kbm` covers v1; a SonicWeave `.swi` importer is a possible later
addition, not a v1 need. Notably Scala is an **import/interchange** format, not the runtime
representation, which stays step-space
([context-owns-resolution](context-owns-resolution.md)). Real-time MTS-ESP / MPE
/ MIDI 2.0 pitch transport are boundary adapters ([osc-only-core](osc-only-core.md)), reachable later
without polluting the core. (In today's engine the shipped tonal context is 12-TET-only; the Scala/EDO
registry rides the same step-space seam and lands with the format & library thread.)

Distilled from: ADR-0008
