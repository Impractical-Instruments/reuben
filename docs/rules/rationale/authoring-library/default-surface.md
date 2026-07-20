# Why: With no surface file, a default surface is auto-derived from the wireable input pipes so every instrument is instantly playable with zero configuration.

[Rule](../../authoring-library.md#default-surface)

Once the interface pipes *are* the boundary ([surface-docs](surface-docs.md)), a surface file is
pure curation — and the goal is that a person can play a freshly-authored instrument immediately,
before anyone has authored one. So with no surface file the renderer synthesizes a default straight
from the pipes: **one widget per wireable input pipe, in declaration order**, the widget inferred
from the pipe's type. This is not a new mechanism — it is exactly what TouchOSC's `boundary`
subcommand already did — promoted to the missing-file fallback.

The inference is honest about what it cannot guess: enum, `note`, `harmony`, and channel-bound
signal pipes are **skipped with a warning naming each**, because a machine default cannot invent
their payloads (which note? which degree?). That is the same degrade-dark discipline
([load-errors-degrade-dark](load-errors-degrade-dark.md)) — a default surface never silently omits a
control, it says which ones it declined.

The payoff is that a new Toy is instantly playable with zero config, and a surface file becomes
something you reach for only to curate, override, or offer a variant — never a prerequisite. Because
the default is derived from the same pipe contract a hand-authored surface binds to, there is one
resolution path with the auto-derive as its base rung, not a separate code path to keep in step.

Distilled from: ADR-0043
