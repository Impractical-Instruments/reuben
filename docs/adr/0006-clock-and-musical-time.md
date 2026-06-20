# Clocking and musical time

## Context

Messages carry sample-accurate timetags (ADR-0001 data model), but musicians work in beats and Toys like groove boxes need tempo. Something must bridge musical time to sample time, and the system must groove together by default while still allowing advanced/generative timing.

## Decision

- **Hybrid clock.** A global default **Clock** exists so any two Toys dropped in a Rig groove together out of the box. Clocks are also Operators, so polytempo, clock division, and independent timing can be patched when wanted. Default sync, optional divergence.
- **Clock provides base musical timing only** — tempo, meter, position. Nothing else.
- **Groove, swing, and feel are separate Operators** that re-time Message streams — composable, per-stream, not buried in the Clock. A default global groove is an on-ramp; different streams can swing differently.
- **Timetags default to musical time**, resolved against the active Clock at dispatch (schedule "beat 2.5" → sample offset). Tempo changes re-time everything for free. Absolute sample-time tags remain available for transport-independent events.

## Considered and rejected

- **Global-only transport:** simple but rigid; polytempo and independent grooves awkward.
- **Fully decentralized clocks:** flexible but nothing syncs by default — beginner-hostile.

## Consequences

- Resolving future musical times under tempo automation needs a tempo map / lookahead; MVP can assume simple/known tempo.
- External tempo sync (Ableton Link, MIDI clock, OSC) feeds the Clock — but only as boundary adapters (see ADR-0007).
