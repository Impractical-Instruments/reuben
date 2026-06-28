//! `modulo` — `out = a mod b` (Euclidean), per sample (ADR-0029, ADR-0017, ADR-0033).
//!
//! The sanctioned way to **wrap a stream into a range**: fold a rising ramp into `[0, b)`, derive a
//! repeating pattern, keep an accumulator bounded. A dense `Float`→`Float` op whose arithmetic is the
//! f32 [`mod_fn`], called once per sample by the signal shell and once per change by the value shell
//! (issue #83).
//!
//! **Euclidean, not the `%` remainder.** [`f32::rem_euclid`] always returns a value in `[0, b)` for a
//! positive modulus `b`, so a negative dividend wraps cleanly (`-1 mod 3 == 2`) — exactly what
//! wrapping phase/CV into a range wants, where the sign-following `%` would emit a negative. A zero
//! modulus would yield `NaN`, so [`mod_fn`] carries an **op-local guard**: `b == 0` produces `0`.
//! Both are `f32`-specific, hence `modulo` lists `numbers: [f32]`.
//!
//! - input 0: `a` (`Float`) — the dividend. Unwired default `0`.
//! - input 1: `b` (`Float`) — the modulus. Unwired default `1`; a `0` modulus yields `0`.
//! - output 0: `out` — `a.rem_euclid(b)`, in `[0, b)` for `b > 0`.

/// The op's scalar math, written once (ADR-0029 pure-fn seam): a Euclidean modulo. The `b == 0`
/// check is `modulo`'s **op-local** guard against a `NaN` poisoning the graph; it lives here.
/// Euclidean (vs `%`) so the result is always non-negative for a positive modulus. `f32`-specific,
/// hence `modulo` is `f32`-only.
#[inline]
fn mod_fn(a: f32, b: f32) -> f32 {
    if b == 0.0 {
        0.0
    } else {
        a.rem_euclid(b)
    }
}

// One declaration -> ModuloF32Value + ModuloF32Signal (ADR-0033). `b` (the modulus) defaults to 1,
// so an unwired modulus wraps `a` into the unit interval `[0, 1)` rather than dividing by zero.
crate::number_operator_contract!(Modulo {
    numbers:  [f32],
    carriers: [value, signal],
    inputs:   { a: number { default 0.0 }, b: number { default 1.0 } },
    outputs:  { out },
    function: mod_fn(a, b),
});

#[cfg(test)]
mod tests {
    use super::modulo_f32_signal::{self, ModuloF32Signal};
    use super::modulo_f32_value::{self, ModuloF32Value};
    use crate::operators::math_test::{signal_out, value_emits};

    /// Drive the signal form; `None` leaves the port unwired (engine materializes its default).
    fn sig(a: Option<&[f32]>, b: Option<&[f32]>, n: usize) -> Vec<f32> {
        signal_out(ModuloF32Signal::new(), modulo_f32_signal::OUT_OUT, n, |d| {
            if let Some(a) = a {
                d.drive(modulo_f32_signal::IN_A, a);
            }
            if let Some(b) = b {
                d.drive(modulo_f32_signal::IN_B, b);
            }
        })
    }

    /// Drive the value form; returns the emitted residue(s).
    fn val(a: Option<f32>, b: Option<f32>) -> Vec<f32> {
        value_emits(ModuloF32Value::new(), |d| {
            if let Some(a) = a {
                d.set(modulo_f32_value::IN_A, a);
            }
            if let Some(b) = b {
                d.set(modulo_f32_value::IN_B, b);
            }
        })
    }

    #[test]
    fn wraps_two_buffers() {
        let a = [7.0, 8.0, 9.0];
        let b = [3.0, 3.0, 3.0];
        assert_eq!(sig(Some(&a), Some(&b), 3), vec![1.0, 2.0, 0.0]);
    }

    #[test]
    fn negative_dividend_wraps_non_negative() {
        // Euclidean: -1 mod 3 == 2, not the `%` remainder -1.
        let a = [-1.0, -2.0, -3.0];
        let b = [3.0, 3.0, 3.0];
        assert_eq!(sig(Some(&a), Some(&b), 3), vec![2.0, 1.0, 0.0]);
    }

    #[test]
    fn unwired_modulus_wraps_into_unit_interval() {
        // `b` defaults to 1, so the result is the non-negative fractional part.
        let a = [2.25, 3.5, -0.25];
        let out = sig(Some(&a), None, 3);
        assert_eq!(out, vec![0.25, 0.5, 0.75]);
    }

    #[test]
    fn zero_modulus_yields_zero_no_nan() {
        let a = [1.0, 2.0, 3.0];
        let b = [0.0, 0.0, 0.0];
        let out = sig(Some(&a), Some(&b), 3);
        assert!(out.iter().all(|s| s.is_finite()));
        assert_eq!(out, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn wraps_held_operands() {
        assert_eq!(val(Some(10.0), Some(4.0)), vec![2.0]);
    }
}
