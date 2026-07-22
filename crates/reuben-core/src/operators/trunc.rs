//! `trunc` — `out = x` with its fractional part dropped, rounding **toward zero**.
//!
//! The symmetric one: `2.9` → `2` and `-2.9` → `-2`, so magnitude only ever shrinks. That makes
//! it the right whole-part for a **bipolar** quantity — the integer part of a signed offset, a
//! detune in whole semitones — where [`floor`](super::floor) would bias every negative value one
//! step further from zero than its positive mirror.
//!
//! Ships in the same three shapes as [`round`](super::round) — an `f32` pair that keeps the type,
//! and an `f32 -> i32` converter that crosses it. See that module for why the crossing needs an
//! explicit operator at all, and why the converter is value-only.
//!
//! - input 0: `x` (`Float`) — the value to truncate. Unwired default `0`.
//! - output 0: `out` — `x` with its fraction dropped, toward zero.

use crate::operators::rounding::RoundInto;

/// The op's scalar math, written once (the pure-fn seam) and generic over the **output** type so
/// the macro can instantiate it per `variants:` entry.
#[inline]
fn trunc_fn<T: RoundInto<U>, U>(x: T) -> U {
    x.trunc_into()
}

// One declaration -> TruncF32Value + TruncF32Signal + TruncF32I32Value.
crate::number_operator_contract!(Trunc {
    variants: [f32 value, f32 signal, f32 -> i32 value],
    inputs:   { x: number { default 0 } },
    outputs:  { out },
    function: trunc_fn(x),
});

#[cfg(test)]
mod tests {
    use super::trunc_f32_i32_value::{self, TruncF32I32Value};
    use super::trunc_f32_signal::{self, TruncF32Signal};
    use super::trunc_f32_value::{self, TruncF32Value};
    use crate::operators::math_test::{i32_value_emits, signal_out, value_emits};

    fn sig(x: &[f32]) -> Vec<f32> {
        signal_out(
            TruncF32Signal::new(),
            trunc_f32_signal::OUT_OUT,
            x.len(),
            |d| {
                d.drive(trunc_f32_signal::IN_X, x);
            },
        )
    }

    // Symmetric about zero — `2.9` and `-2.9` both lose their fraction rather than one of them
    // moving a whole step. That symmetry is the entire difference from `floor`.
    #[test]
    fn truncates_a_buffer_toward_zero_symmetrically() {
        assert_eq!(
            sig(&[-2.9, -0.9, 0.0, 0.9, 2.9]),
            vec![-2.0, -0.0, 0.0, 0.0, 2.0]
        );
    }

    #[test]
    fn unwired_input_is_silent() {
        let out = signal_out(TruncF32Signal::new(), trunc_f32_signal::OUT_OUT, 4, |_| {});
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn truncates_held_value() {
        let out = value_emits(TruncF32Value::new(), |d| {
            d.set(trunc_f32_value::IN_X, -2.9);
        });
        assert_eq!(out, vec![-2.0]);
    }

    #[test]
    fn converter_emits_a_genuine_i32() {
        let out = i32_value_emits(TruncF32I32Value::new(), |d| {
            d.set(trunc_f32_i32_value::IN_X, -2.9);
        });
        assert_eq!(out, vec![-2]);
    }
}
