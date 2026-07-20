# Why: Message timetags default to musical time, resolved to a sample offset against the active Clock at dispatch, with absolute sample-time tags available for transport-independent events.

[Rule](../../signal-time-dsp.md#musical-timetags)

Messages carry sample-accurate timestamps, but musicians work in beats. If events were stamped in
sample time only, a tempo change would leave every scheduled event stranded at its old wall-clock
moment. So a timetag **defaults to musical time** ("beat 2.5") and is resolved to a sample offset
against the active Clock at dispatch — and because the resolution happens against the current Clock,
a tempo change **re-times everything for free**. Absolute sample-time tags stay available for the
minority of events that must be transport-independent (a fixed-latency effect trigger, a
sample-accurate one-shot), so nothing is lost by defaulting the other way.

The default is musical because that is what almost every musical event wants; the escape hatch exists
because a few do not. Resolving *future* musical times under tempo automation needs a tempo
map / lookahead — the MVP assumes a simple, known tempo and defers the general lookahead. External
tempo sources (Link, MIDI clock, OSC) reach this only as boundary adapters feeding the Clock
([osc-only-core](osc-only-core.md)), never as a second timing authority in the core.

Distilled from: ADR-0006
