//! `ceil` — `out = ` the least whole number `>= x`.
//!
//! Rounding **up**, always, negatives included (`-2.5` → `-2`). The "how many whole units does
//! this cover" answer: a fractional length in beats ceilings to the number of bars that must be
//! allocated, a partial step to the step that finishes it.
//!
//! Ships in the same three shapes as [`round`](super::round) — an `f32` pair that keeps the type,
//! and an `f32 -> i32` converter that crosses it. See that module for why the crossing needs an
//! explicit operator at all, and why the converter is value-only.
//!
//! - input 0: `x` (`Float`) — the value to raise. Unwired default `0`.
//! - output 0: `out` — the least whole number `>= x`.

use crate::operators::rounding::RoundInto;

/// The op's scalar math, written once (the pure-fn seam) and generic over the **output** type so
/// the macro can instantiate it per `variants:` entry.
#[inline]
fn ceil_fn<T: RoundInto<U>, U>(x: T) -> U {
    x.ceil_into()
}

// One declaration -> CeilF32Value + CeilF32Signal + CeilF32I32Value.
crate::number_operator_contract!(Ceil {
    variants: [f32 value, f32 signal, f32 -> i32 value],
    inputs:   { x: number { default 0 } },
    outputs:  { out },
    function: ceil_fn(x),
});

#[cfg(test)]
mod tests {
    use super::ceil_f32_i32_value::{self, CeilF32I32Value};
    use super::ceil_f32_signal::{self, CeilF32Signal};
    use super::ceil_f32_value::{self, CeilF32Value};
    use crate::operators::math_test::{i32_value_emits, signal_out, value_emits};

    fn sig(x: &[f32]) -> Vec<f32> {
        signal_out(
            CeilF32Signal::new(),
            ceil_f32_signal::OUT_OUT,
            x.len(),
            |d| {
                d.drive(ceil_f32_signal::IN_X, x);
            },
        )
    }

    // Up in both directions: `-2.9` ceilings to `-2`, toward zero here, which is where it parts
    // company with `round` and agrees with `trunc`.
    #[test]
    fn raises_a_buffer_upward_including_negatives() {
        assert_eq!(
            sig(&[-2.9, -0.5, 0.0, 0.1, 2.1]),
            vec![-2.0, -0.0, 0.0, 1.0, 3.0]
        );
    }

    #[test]
    fn unwired_input_is_silent() {
        let out = signal_out(CeilF32Signal::new(), ceil_f32_signal::OUT_OUT, 4, |_| {});
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn raises_held_value() {
        let out = value_emits(CeilF32Value::new(), |d| {
            d.set(ceil_f32_value::IN_X, 2.1);
        });
        assert_eq!(out, vec![3.0]);
    }

    #[test]
    fn converter_emits_a_genuine_i32() {
        let out = i32_value_emits(CeilF32I32Value::new(), |d| {
            d.set(ceil_f32_i32_value::IN_X, 2.1);
        });
        assert_eq!(out, vec![3]);
    }
}
