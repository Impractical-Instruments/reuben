//! `floor` — `out = ` the greatest whole number `<= x`.
//!
//! Rounding **down**, always, negatives included (`-2.5` → `-3`). The step-index quantizer: a
//! position swept across `0.0..n` floors to a uniform bucket per step, where
//! [`round`](super::round) would make the first and last buckets half-width. Also the honest
//! "which beat are we in" from a fractional beat position.
//!
//! Ships in the same three shapes as [`round`](super::round) — an `f32` pair that keeps the type,
//! and an `f32 -> i32` converter that crosses it. See that module for why the crossing needs an
//! explicit operator at all, and why the converter is value-only.
//!
//! - input 0: `x` (`Float`) — the value to floor. Unwired default `0`.
//! - output 0: `out` — the greatest whole number `<= x`.

use crate::operators::rounding::RoundInto;

/// The op's scalar math, written once (the pure-fn seam) and generic over the **output** type so
/// the macro can instantiate it per `variants:` entry.
#[inline]
fn floor_fn<T: RoundInto<U>, U>(x: T) -> U {
    x.floor_into()
}

// One declaration -> FloorF32Value + FloorF32Signal + FloorF32I32Value.
crate::number_operator_contract!(Floor {
    variants: [f32 value, f32 signal, f32 -> i32 value],
    inputs:   { x: number { default 0 } },
    outputs:  { out },
    function: floor_fn(x),
});

#[cfg(test)]
mod tests {
    use super::floor_f32_i32_value::{self, FloorF32I32Value};
    use super::floor_f32_signal::{self, FloorF32Signal};
    use super::floor_f32_value::{self, FloorF32Value};
    use crate::operators::math_test::{i32_value_emits, signal_out, value_emits};

    fn sig(x: &[f32]) -> Vec<f32> {
        signal_out(
            FloorF32Signal::new(),
            floor_f32_signal::OUT_OUT,
            x.len(),
            |d| {
                d.drive(floor_f32_signal::IN_X, x);
            },
        )
    }

    // Down in both directions: `-2.1` floors to `-3`, not toward zero. That is the whole
    // difference from `trunc`, and it only shows on negatives.
    #[test]
    fn floors_a_buffer_downward_including_negatives() {
        assert_eq!(
            sig(&[-2.1, -0.5, 0.0, 0.9, 2.9]),
            vec![-3.0, -1.0, 0.0, 0.0, 2.0]
        );
    }

    #[test]
    fn unwired_input_is_silent() {
        let out = signal_out(FloorF32Signal::new(), floor_f32_signal::OUT_OUT, 4, |_| {});
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn floors_held_value() {
        let out = value_emits(FloorF32Value::new(), |d| {
            d.set(floor_f32_value::IN_X, -2.1);
        });
        assert_eq!(out, vec![-3.0]);
    }

    #[test]
    fn converter_emits_a_genuine_i32() {
        let out = i32_value_emits(FloorF32I32Value::new(), |d| {
            d.set(floor_f32_i32_value::IN_X, -2.1);
        });
        assert_eq!(out, vec![-3]);
    }
}
