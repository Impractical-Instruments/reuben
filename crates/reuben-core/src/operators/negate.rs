//! `negate` — `out = -x`, per sample (ADR-0029, ADR-0017, ADR-0033).
//!
//! The sanctioned way to **invert a stream**: flip the sign of a CV, phase-invert audio, turn a
//! rising envelope into a falling one. A dense `Float`→`Float` unary op whose arithmetic is the
//! generic [`negate_fn`], called once per sample by the signal shell and once per change by the value
//! shell (issue #83).
//!
//! - input 0: `x` (`Float`) — the value to negate. Unwired default `0`.
//! - output 0: `out` — `-x`.

/// The op's scalar math, written once (ADR-0029 pure-fn seam) and generic over the number type so
/// the macro can instantiate it per `numbers` entry (`f32` today).
#[inline]
fn negate_fn<T: core::ops::Neg<Output = T>>(x: T) -> T {
    -x
}

// One declaration -> NegateF32Value + NegateF32Signal (ADR-0033). A pure unary sign flip; `x`
// defaults to 0, so an unwired input is silent (`-0 == 0`).
crate::number_operator_contract!(Negate {
    numbers:  [f32],
    carriers: [value, signal],
    inputs:   { x: number { default 0.0 } },
    outputs:  { out },
    function: negate_fn(x),
});

#[cfg(test)]
mod tests {
    use super::negate_f32_signal::{self, NegateF32Signal};
    use super::negate_f32_value::{self, NegateF32Value};
    use crate::operators::math_test::{signal_out, value_emits};

    fn sig(x: &[f32]) -> Vec<f32> {
        signal_out(
            NegateF32Signal::new(),
            negate_f32_signal::OUT_OUT,
            x.len(),
            |d| {
                d.drive(negate_f32_signal::IN_X, x);
            },
        )
    }

    #[test]
    fn negates_a_buffer() {
        assert_eq!(sig(&[-1.0, 0.0, 2.0, -3.0]), vec![1.0, -0.0, -2.0, 3.0]);
    }

    #[test]
    fn unwired_input_is_silent() {
        let out = signal_out(
            NegateF32Signal::new(),
            negate_f32_signal::OUT_OUT,
            4,
            |_| {},
        );
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn negates_held_value() {
        let out = value_emits(NegateF32Value::new(), |d| {
            d.set(negate_f32_value::IN_X, 4.0);
        });
        assert_eq!(out, vec![-4.0]);
    }
}
