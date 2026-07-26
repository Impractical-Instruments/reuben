# Why: Curve and shaping ops are named for their precise math — power is x^exponent — each its own operator rather than a generic curve knob with a mode param.

[Rule](../../signal-time-dsp.md#curve-ops-named-for-math)

Once the envelope emits a linear contour ([envelope-emits-cv](envelope-emits-cv.md)), *something* has
to shape it — and the tempting move is one `curve` operator with a mode knob. That is rejected: a
generic knob hides which math is running and turns every future shape into a mode flag on a
grab-bag op. Instead a shaping op is **named for the precise math it applies**. The first is `power`,
`out = x^exponent` — a *power* curve, and the name stays honest. Future shapes (`logarithmic`, …) each
get their own named op rather than overloading one operator with a mode param.

Why `x^k` and not a true exponential `e^{kx}`: both track perceived loudness far better than linear,
but `x^k` maps `0 → 0` and `1 → 1` exactly, so a release reaches **true silence** and a peak reaches
**unity** with no floor parameter to fudge — and it is cheaper (one `powf`, no renormalization). `x²`
is perceptually close to an exponential decay across the audible range. A true `e^{kx}` never reaches
0 and would need a −60 dB-style floor plus renormalization to be usable as a release; the power curve
avoids that entirely. `power` is unipolar with an **op-local** NaN guard — negatives clamp to 0 so a
fractional exponent can't yield NaN — living in the op's own scalar fn, inherited by nobody.
`exponent` is a materialized operand carrying its range guard, default, and UI knob while staying
wire-able; in the signal carrier it is read per sample as a uniform buffer operand, which is
functionally identical for a held control. This is the template every curve op follows: one file, a
metadata-bearing shaping operand, op-local guards.

Distilled from: ADR-0027, ADR-0029
