# Why: An interface override may narrow what a port advertises but never widen it past what the engine enforces.

[Rule](../../authoring-library.md#advertised-range-is-a-subset)

An interface override lets an instrument re-present an inner port to whoever plays it: give it a
unit, narrow its range to the part that is musically useful. That last
one is a [Good Button](good-button.md) mechanism — a filter whose cutoff sweeps the whole audible
range is technically correct and practically unplayable, and narrowing the advertised range to the
part that sounds good is how a control becomes hard to make sound bad.

Narrowing is safe because every value the surface can now produce is one the engine already
accepted. Widening is not: the extremes do nothing — clamped or rejected below the surface — so the
knob moves, the display updates, and the player's model of the instrument silently stops matching
it. So the advertised range must be a **non-inverted subset** of the inner port's engine-enforced
range; an override on a port with no numeric range is refused outright rather than silently ignored,
and an inverted or empty range (`min >= max`) describes a control with no positions and is refused
by the same check.

The rule splits the override fields by whether they *can* lie. Range and effective-default can, so
they are validated — the effective default additionally has to sit inside the advertised range, or
the control starts at a position it cannot be returned to. `unit` cannot: it re-presents, and no
value of it makes the engine accept or reject anything different, so it stays unconstrained.
Display name and widget are not overrides at all any more — they are the surface doc's
([surface-docs](surface-docs.md)), drained from a pipe with a deprecation warning.

Where a v2 input pipe **owns its range outright** there is nothing to be a subset of, and the range
is validated where it is declared instead. The law governs overrides — entries that re-present
something that already has a range — not declarations.

Decided in: issue #639 — settled directly, no ADR.
