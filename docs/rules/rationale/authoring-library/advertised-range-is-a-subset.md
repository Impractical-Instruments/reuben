# Why: An interface override may narrow what a port advertises but never widen it past what the engine enforces.

[Rule](../../authoring-library.md#advertised-range-is-a-subset)

An interface override lets an instrument re-present an inner port to whoever plays it: rename it,
give it a unit, choose a widget, narrow its range to the part that is musically useful. That last
one is a [Good Button](good-button.md) mechanism — a filter whose cutoff sweeps the whole audible
range is technically correct and practically unplayable, and narrowing the advertised range to the
part that sounds good is how a control becomes hard to make sound bad.

Narrowing is safe because every value the surface can now produce is one the engine already
accepted. Widening is not, and it is the case the rule exists to refuse. An override advertising a
range wider than the engine enforces produces a control whose extremes do nothing — the value is
clamped, or rejected, somewhere below the surface — and the symptom is the worst kind: a knob that
moves, a display that updates, and a sound that stops responding partway up. The player's model of
the instrument silently stops matching the instrument. So the advertised range must be a
**non-inverted subset** of the inner port's engine-enforced range, and an override on a port with no
numeric range at all is refused outright rather than being silently ignored.

The inverted and empty cases are refused by the same check for the same reason: a range whose
minimum meets or exceeds its maximum describes a control with no positions, which is never what
anyone meant and is always a typo worth reporting at load.

The rule splits the override fields by whether they *can* lie. Range and effective-default can, so
they are validated — the effective default additionally has to sit inside the advertised range, or
the control starts at a position it cannot be returned to. Label, unit, and widget cannot: they
rename and re-present, and no value of them makes the engine accept or reject anything different. So
they stay unconstrained, and the validation stays narrow enough to be obviously about one thing.

Where a v2 input pipe **owns its range outright** there is nothing to be a subset of, and the range
is validated where it is declared instead. The law governs overrides — entries that re-present
something that already has a range — not declarations.

Decided in: issue #639 — settled directly, no ADR.
