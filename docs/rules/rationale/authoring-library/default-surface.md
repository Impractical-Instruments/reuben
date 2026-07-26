# Why: With no surface file, a default surface is auto-derived from the wireable input pipes so every instrument is instantly playable with zero configuration.

[Rule](../../authoring-library.md#default-surface)

Once the interface pipes *are* the boundary ([surface-docs](surface-docs.md)), a surface file is
pure curation — and the goal is that a person can play a freshly-authored instrument immediately,
before anyone has authored one. So with no surface file the renderer synthesizes a default straight
from the pipes: **one fader per wireable input pipe, in declaration order** — the fader-able types
being `f32`, `i32`, and an `f32_buffer` pipe that declares both bounds. This is not a new mechanism
— it is exactly what the TouchOSC generator's `boundary` subcommand already did — promoted to the
missing-file fallback.

The inference is honest about what it cannot guess: channel-bound pipes (a device binding, not a
control), bare `f32_buffer` pipes with no range to scale a fader into, and enum/`note`/`harmony`
pipes are **skipped with a warning naming each**, because a machine default cannot invent their
payloads (which note? which degree?). That is the same degrade-dark discipline
([load-errors-degrade-dark](load-errors-degrade-dark.md)) — a default surface never silently omits a
control, it says which ones it declined.

Because the default derives from the same pipe contract a hand-authored surface binds to, there is
one resolution path with the auto-derive as its base rung, not a second code path to keep in step.

Distilled from: ADR-0043
