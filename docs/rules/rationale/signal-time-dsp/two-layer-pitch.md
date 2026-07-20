# Why: Pitch is a two-layer model — a symbolic degree within the active Scale (with float MIDI available as a 12-TET coordinate) resolved to Hz by a Tuning — and 12-TET is just the default Tuning.

[Rule](../../signal-time-dsp.md#two-layer-pitch)

reuben must fully support non-Western tonalities and user-definable tunings, not just 12-TET. A survey
of serious practice found that *every* serious engine separates **symbolic pitch identity** from
**frequency resolution**; 12-TET is never baked in. So pitch is two layers: a **symbolic** layer —
scale degree within the active Scale (primary), with a float MIDI note (60.0 = middle C) available as
a 12-TET coordinate — and a **resolution** layer where a **Tuning** maps symbolic pitch to Hz, which
is what oscillators consume. 12-TET is just the default Tuning.

Scale-degree-primary (rather than float-MIDI-primary or Hz-primary) is the load-bearing choice: it
gives **free transposition** and the "always in key" snap for free, which raw Hz loses (no musical
meaning, no snap) and which a MIDI-note primary blunts. The degree is symbolic and re-spells live when
the key/scale changes, precisely because it is *not* a frequency until the Tuning resolves it. In the
engine `Pitch` is an enum — `Degree(i32)` **or** `Absolute(f32)`, never both and never neither (the
old `{ degree: Option, midi }` struct had invalid states) — and `Pitch` never holds a frequency
itself; `Tuning::hz` (or the tonal context's `hz`) is the only place Hz appears. A bare degree with
no context to resolve it falls back to a chromatic reading from middle C, but real degree resolution
goes through the active tonal context ([context-owns-resolution](context-owns-resolution.md)), which
is what makes a key change re-spell a whole line.

Distilled from: ADR-0008
