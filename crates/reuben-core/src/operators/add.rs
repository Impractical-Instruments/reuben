//! `add` — `out = a + b`, per sample.
//!
//! Base-plus-modulation is an explicit `add`: both operands' unwired default is the additive
//! identity `0`, so wiring only one side passes it through unchanged (an unwired Value
//! materializes to its default at the Signal input).
//!
//! - input 0: `a` (`Float`) — first operand. Unwired default `0`.
//! - input 1: `b` (`Float`) — second operand. Unwired default `0`.
//! - output 0: `out` — `a + b`.
//!
//! see rules: composition-operators

use crate::operators::pointwise::PointwiseNum;

/// The op's scalar math, generic over the number type via [`PointwiseNum`] so the macro
/// instantiates it per `variants:` entry.
///
/// [`PointwiseNum`] rather than `core::ops::Add`: the sum must be total at every instantiated type,
/// and `i32`'s `+` panics on overflow in a debug build (a render-thread panic) where `f32`'s
/// yields `inf`.
#[inline]
fn add_fn<T: PointwiseNum>(a: T, b: T) -> T {
    a.add(b)
}

// One declaration -> AddF32Value + AddF32Signal, each with its contract, Operator impl,
// registration, and a defaults-are-data test. The additive identity `0` is the operands'
// unwired default, so wiring only one side passes it through. Port names `a`/`b` are
// preserved from the hand-written contract, so existing instrument wiring is untouched.
crate::number_operator_contract!(Add {
    variants: [f32 value, f32 signal, i32 value],
    inputs:   { a: number { default 0.0 }, b: number { default 0.0 } },
    outputs:  { out },
    function: add_fn(a, b),
});

#[cfg(test)]
mod tests {
    use super::add_f32_signal::{self, AddF32Signal};
    use super::add_f32_value::{self, AddF32Value};
    use super::add_i32_value::{self, AddI32Value};
    use crate::operators::math_test::{i32_value_emits, signal_out, value_emits};

    /// Drive the signal form through the real engine; returns `out`. `Some(buf)` drives a buffer,
    /// `None` leaves the port unwired so the engine materializes its additive-identity default `0`.
    fn sig(a: Option<&[f32]>, b: Option<&[f32]>, n: usize) -> Vec<f32> {
        signal_out(AddF32Signal::new(), add_f32_signal::OUT_OUT, n, |d| {
            if let Some(a) = a {
                d.drive(add_f32_signal::IN_A, a);
            }
            if let Some(b) = b {
                d.drive(add_f32_signal::IN_B, b);
            }
        })
    }

    /// Drive the value form; returns the emitted sum(s). `None` leaves the operand at its default.
    fn val(a: Option<f32>, b: Option<f32>) -> Vec<f32> {
        value_emits(AddF32Value::new(), |d| {
            if let Some(a) = a {
                d.set(add_f32_value::IN_A, a);
            }
            if let Some(b) = b {
                d.set(add_f32_value::IN_B, b);
            }
        })
    }

    #[test]
    fn sums_two_buffers() {
        let a = [1.0, 2.0, 3.0];
        let b = [10.0, 20.0, 30.0];
        assert_eq!(sig(Some(&a), Some(&b), 3), vec![11.0, 22.0, 33.0]);
    }

    #[test]
    fn unwired_b_passes_a_through() {
        // Additive identity 0: wiring only `in_a` leaves it unchanged (base-plus-modulation).
        let a = [5.0, 6.0, 7.0];
        assert_eq!(sig(Some(&a), None, 3), vec![5.0, 6.0, 7.0]);
    }

    #[test]
    fn sums_held_operands() {
        assert_eq!(val(Some(3.0), Some(4.0)), vec![7.0]);
    }

    #[test]
    fn unwired_b_passes_held_a_through() {
        // Additive identity 0: wiring only `in_a` emits it unchanged.
        assert_eq!(val(Some(5.0), None), vec![5.0]);
    }

    /// Drive the `i32` value form; returns the emitted sum(s) as integers.
    fn val_i32(a: Option<i32>, b: Option<i32>) -> Vec<i32> {
        i32_value_emits(AddI32Value::new(), |d| {
            if let Some(a) = a {
                d.set(add_i32_value::IN_A, a);
            }
            if let Some(b) = b {
                d.set(add_i32_value::IN_B, b);
            }
        })
    }

    // The contract assertion: the `i32` instantiation runs the same `add_fn` at the integer type,
    // through the real engine, and the emit is a genuine `Arg::I32`.
    #[test]
    fn sums_held_i32_operands() {
        assert_eq!(val_i32(Some(3), Some(4)), vec![7]);
        assert_eq!(val_i32(Some(-5), Some(2)), vec![-3]);
        // Same additive identity as the f32 carriers: an unwired operand passes the other through.
        assert_eq!(val_i32(Some(5), None), vec![5]);
    }

    // Integer overflow saturates rather than panicking (debug) or wrapping (release) — `process`
    // runs on the render thread, where a panic is fatal. See `operators::pointwise`.
    #[test]
    fn i32_sum_saturates_rather_than_overflowing() {
        assert_eq!(val_i32(Some(i32::MAX), Some(1)), vec![i32::MAX]);
        assert_eq!(val_i32(Some(i32::MIN), Some(-1)), vec![i32::MIN]);
    }
}
