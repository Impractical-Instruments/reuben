//! `reciprocal` — `out = 1 / x`, per sample (ADR-0029, ADR-0017, ADR-0033).
//!
//! The sanctioned way to **invert a stream multiplicatively**: turn a ratio into its inverse, a
//! frequency into a period, a rate into a time. A dense `Float`→`Float` unary op whose arithmetic is
//! the f32 [`recip_fn`], called once per sample by the signal shell and once per change by the value
//! shell (issue #83).
//!
//! Taking the reciprocal of zero would yield `±inf`, so [`recip_fn`] carries an **op-local guard**: a
//! zero input produces `0` (a finite result) rather than infinity. That `f32`-specific guard is why
//! `reciprocal` lists `numbers: [f32]`.
//!
//! - input 0: `x` (`Float`) — the value to invert. Unwired default `1` (so `1/1 == 1`, the identity).
//! - output 0: `out` — `1 / x` (or `0` when `x == 0`).

/// The op's scalar math, written once (ADR-0029 pure-fn seam). The `x == 0` check is `reciprocal`'s
/// **op-local** guard against an `inf` poisoning the graph; it lives here. `f32`-specific, hence
/// `reciprocal` is `f32`-only.
#[inline]
fn recip_fn(x: f32) -> f32 {
    if x == 0.0 {
        0.0
    } else {
        x.recip()
    }
}

// One declaration -> ReciprocalF32Value + ReciprocalF32Signal (ADR-0033). `x` defaults to 1 (the
// multiplicative identity) so an unwired input emits 1 rather than the guarded zero.
crate::number_operator_contract!(Reciprocal {
    numbers:  [f32],
    carriers: [value, signal],
    inputs:   { x: number { default 1.0 } },
    outputs:  { out },
    function: recip_fn(x),
});

#[cfg(test)]
mod tests {
    use super::reciprocal_f32_signal::{self, ReciprocalF32Signal};
    use super::reciprocal_f32_value::{self, ReciprocalF32Value};
    use crate::operators::math_test::{signal_out, value_emits};

    fn sig(x: &[f32]) -> Vec<f32> {
        signal_out(
            ReciprocalF32Signal::new(),
            reciprocal_f32_signal::OUT_OUT,
            x.len(),
            |d| {
                d.drive(reciprocal_f32_signal::IN_X, x);
            },
        )
    }

    #[test]
    fn inverts_a_buffer() {
        assert_eq!(sig(&[2.0, 4.0, 0.5, -1.0]), vec![0.5, 0.25, 2.0, -1.0]);
    }

    #[test]
    fn zero_input_yields_zero_no_inf() {
        let out = sig(&[0.0, 0.0]);
        assert!(out.iter().all(|s| s.is_finite()));
        assert_eq!(out, vec![0.0, 0.0]);
    }

    #[test]
    fn inverts_held_value() {
        let out = value_emits(ReciprocalF32Value::new(), |d| {
            d.set(reciprocal_f32_value::IN_X, 4.0);
        });
        assert_eq!(out, vec![0.25]);
    }

    #[test]
    fn held_zero_yields_zero() {
        let out = value_emits(ReciprocalF32Value::new(), |d| {
            d.set(reciprocal_f32_value::IN_X, 0.0);
        });
        assert_eq!(out, vec![0.0]);
    }
}
