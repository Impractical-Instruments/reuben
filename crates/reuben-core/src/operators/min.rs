//! `min` — `out = min(a, b)`, per sample (ADR-0029, ADR-0017, ADR-0033).
//!
//! The sanctioned way to **take the lower of two streams**: a ceiling on CV, the upper half of a
//! clamp, a duck/sidechain floor. A dense `Float`→`Float` op whose arithmetic is the f32 [`min_fn`],
//! called once per sample by the signal shell and once per change by the value shell (issue #83).
//!
//! Unlike add/mul there is no finite identity, so `b`'s unwired default is the **range maximum**
//! (`+1e6`): `min(a, +1e6) == a` for any in-range signal, so wiring only `a` passes it through. Uses
//! [`f32::min`] (NaN-ignoring), hence `min` lists `numbers: [f32]`.
//!
//! - input 0: `a` (`Float`) — first operand. Unwired default `0`.
//! - input 1: `b` (`Float`) — second operand. Unwired default `+1e6` (the range max — a no-op).
//! - output 0: `out` — `min(a, b)`.

/// The op's scalar math, written once (ADR-0029 pure-fn seam): [`f32::min`], which ignores `NaN`
/// operands. `f32`-specific, hence `min` is `f32`-only.
#[inline]
fn min_fn(a: f32, b: f32) -> f32 {
    a.min(b)
}

// One declaration -> MinF32Value + MinF32Signal (ADR-0033). `b` defaults to the range maximum so an
// unwired second operand is a no-op (`min(a, +1e6) == a`), passing `a` through (ADR-0029).
crate::number_operator_contract!(Min {
    numbers:  [f32],
    carriers: [value, signal],
    inputs:   { a: number { default 0.0 }, b: number { default 1000000.0 } },
    outputs:  { out },
    function: min_fn(a, b),
});

#[cfg(test)]
mod tests {
    use super::min_f32_signal::{self, MinF32Signal};
    use super::min_f32_value::{self, MinF32Value};
    use crate::operators::math_test::{signal_out, value_emits};

    /// Drive the signal form; `None` leaves the port unwired (engine materializes its default).
    fn sig(a: Option<&[f32]>, b: Option<&[f32]>, n: usize) -> Vec<f32> {
        signal_out(MinF32Signal::new(), min_f32_signal::OUT_OUT, n, |d| {
            if let Some(a) = a {
                d.drive(min_f32_signal::IN_A, a);
            }
            if let Some(b) = b {
                d.drive(min_f32_signal::IN_B, b);
            }
        })
    }

    /// Drive the value form; returns the emitted minimum(s).
    fn val(a: f32, b: f32) -> Vec<f32> {
        value_emits(MinF32Value::new(), |d| {
            d.set(min_f32_value::IN_A, a);
            d.set(min_f32_value::IN_B, b);
        })
    }

    #[test]
    fn takes_lower_of_two_buffers() {
        let a = [1.0, 5.0, 3.0];
        let b = [4.0, 2.0, 3.0];
        assert_eq!(sig(Some(&a), Some(&b), 3), vec![1.0, 2.0, 3.0]);
    }

    #[test]
    fn unwired_b_passes_a_through() {
        // b defaults to the range max, a no-op for in-range signals.
        let a = [-5.0, 0.0, 7.0];
        assert_eq!(sig(Some(&a), None, 3), vec![-5.0, 0.0, 7.0]);
    }

    #[test]
    fn takes_lower_of_held_operands() {
        assert_eq!(val(3.0, 8.0), vec![3.0]);
        assert_eq!(val(8.0, 3.0), vec![3.0]);
    }
}
