//! `round` — `out = x` to the nearest whole number, halves away from zero.
//!
//! The **quantizer**: the sanctioned way to turn a continuous value into a whole one. Snapping a
//! swept control to integer steps, turning a float-MIDI pitch into a note number, taking a
//! modulated value to a step index.
//!
//! It ships in two shapes because "whole" and "integer-typed" are different needs:
//!
//! - `round_f32_value` / `round_f32_signal` — the value becomes whole, the type stays `f32`. The
//!   ordinary dense unary op, for a control or a stream that should move in steps but still feeds
//!   `f32` ports.
//! - `round_f32_i32_value` — the **converter**, whose output port is genuinely `i32`. This is the
//!   explicit path across the one numeric crossing the wire check refuses: `i32 -> f32` widens
//!   implicitly, `f32 -> i32` is lossy and needs a rounding *decision*, so it takes an operator
//!   that names which decision (see `per-wire-form-check`). `floor`/`ceil`/`trunc` are the other
//!   three answers.
//!
//! There is no `f32 -> i32 signal`: `i32` has no dense buffer form (issue #560), so every
//! converter is value-only.
//!
//! - input 0: `x` (`Float`) — the value to round. Unwired default `0`.
//! - output 0: `out` — `x` rounded to nearest, halves away from zero (`-2.5` → `-3`, `2.5` → `3`).

use crate::operators::rounding::RoundInto;

/// The op's scalar math, written once (the pure-fn seam) and generic over the **output** type so
/// the macro can instantiate it per `variants:` entry — the same body serves the `f32` rounding
/// and the `i32` converter.
#[inline]
fn round_fn<T: RoundInto<U>, U>(x: T) -> U {
    x.round_into()
}

// One declaration -> RoundF32Value + RoundF32Signal + RoundF32I32Value. `x` defaults to 0, so an
// unwired input is silent and rounds to zero in either output type.
crate::number_operator_contract!(Round {
    variants: [f32 value, f32 signal, f32 -> i32 value],
    inputs:   { x: number { default 0 } },
    outputs:  { out },
    function: round_fn(x),
});

#[cfg(test)]
mod tests {
    use super::round_f32_i32_value::{self, RoundF32I32Value};
    use super::round_f32_signal::{self, RoundF32Signal};
    use super::round_f32_value::{self, RoundF32Value};
    use crate::operators::math_test::{i32_value_emits, signal_out, value_emits};

    fn sig(x: &[f32]) -> Vec<f32> {
        signal_out(
            RoundF32Signal::new(),
            round_f32_signal::OUT_OUT,
            x.len(),
            |d| {
                d.drive(round_f32_signal::IN_X, x);
            },
        )
    }

    // Halves go away from zero in both directions — the property that distinguishes `round` from
    // the other three modes, and the one a positive-only test would miss.
    #[test]
    fn rounds_a_buffer_to_nearest_with_halves_away_from_zero() {
        assert_eq!(
            sig(&[-2.5, -0.4, 0.0, 0.5, 2.5, 7.3]),
            vec![-3.0, -0.0, 0.0, 1.0, 3.0, 7.0]
        );
    }

    #[test]
    fn unwired_input_is_silent() {
        let out = signal_out(RoundF32Signal::new(), round_f32_signal::OUT_OUT, 4, |_| {});
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn rounds_held_value() {
        let out = value_emits(RoundF32Value::new(), |d| {
            d.set(round_f32_value::IN_X, -2.5);
        });
        assert_eq!(out, vec![-3.0]);
    }

    /// The converter emits `Arg::I32`, which is the assertion that matters: an operator that only
    /// *named* itself a converter would emit `Arg::F32` here and panic in `i32_emit`.
    #[test]
    fn converter_emits_a_genuine_i32() {
        let out = i32_value_emits(RoundF32I32Value::new(), |d| {
            d.set(round_f32_i32_value::IN_X, 2.5);
        });
        assert_eq!(out, vec![3]);
    }

    /// `process` runs on the render thread, so the converter must answer for every input its port
    /// can carry rather than trap. `OpDriver::set` writes the latch directly, bypassing the range
    /// clamp — the only way a test can reach the saturating arm at all.
    #[test]
    fn converter_saturates_rather_than_trapping() {
        let at = |x: f32| {
            i32_value_emits(RoundF32I32Value::new(), |d| {
                d.set(round_f32_i32_value::IN_X, x);
            })
        };
        assert_eq!(at(f32::INFINITY), vec![i32::MAX]);
        assert_eq!(at(f32::NEG_INFINITY), vec![i32::MIN]);
        assert_eq!(at(f32::NAN), vec![0]);
    }
}
