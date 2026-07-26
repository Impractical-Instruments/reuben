# Why: Every arithmetic op is its own module over a pure scalar fn, and the shared `math.rs`/`Number`-trait core is deleted rather than re-bounded.

[Rule](../../signal-time-dsp.md#math-family-dense-float)

A `Number` trait in a `math.rs` file drew a boundary nothing could state: `power` is unambiguously
math yet the symmetric-binary macro could not emit it, so which ops lived in the file was a coin-flip.
The fix was deletion, not a better boundary — `math.rs`, the `Number` trait, and `signal_pointwise!`
are gone, and every arithmetic op is its own module whose scalar math is a pure fn. Which carriers
that fn is instantiated at is settled by
[pointwise-number-operators](../composition-operators/pointwise-number-operators.md).

Distilled from: ADR-0029
