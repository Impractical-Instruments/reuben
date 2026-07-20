//! `abs` — `out = |x|`, per sample.
//!
//! The sanctioned way to **rectify a stream**: full-wave rectification of audio, taking the
//! magnitude of a bipolar CV, folding a signal into the positive half-plane. A dense `Float`→`Float`
//! unary op whose arithmetic is the generic [`abs_fn`], called once per sample by the signal shell
//! and once per change by the value shell (issue #83). Needs only [`Signed`](num_traits::Signed), so
//! it is generic over any signed number type.
//!
//! - input 0: `x` (`Float`) — the value to rectify. Unwired default `0`.
//! - output 0: `out` — `|x|`.

/// The op's scalar math, written once (the pure-fn seam) and generic over any signed number:
/// [`Signed::abs`](num_traits::Signed::abs), which delegates to `f32::abs` for the `f32` instance.
#[inline]
fn abs_fn<T: num_traits::Signed>(x: T) -> T {
    x.abs()
}

// One declaration -> AbsF32Value + AbsF32Signal. A pure unary magnitude; `x` defaults to
// 0, so an unwired input is silent.
crate::number_operator_contract!(Abs {
    numbers:  [f32],
    carriers: [value, signal],
    inputs:   { x: number { default 0.0 } },
    outputs:  { out },
    function: abs_fn(x),
});

#[cfg(test)]
mod tests {
    use super::abs_f32_signal::{self, AbsF32Signal};
    use super::abs_f32_value::{self, AbsF32Value};
    use crate::operators::math_test::{signal_out, value_emits};

    fn sig(x: &[f32]) -> Vec<f32> {
        signal_out(AbsF32Signal::new(), abs_f32_signal::OUT_OUT, x.len(), |d| {
            d.drive(abs_f32_signal::IN_X, x);
        })
    }

    #[test]
    fn rectifies_a_buffer() {
        assert_eq!(
            sig(&[-1.0, -0.5, 0.0, 0.5, 1.0]),
            vec![1.0, 0.5, 0.0, 0.5, 1.0]
        );
    }

    #[test]
    fn unwired_input_is_silent() {
        let out = signal_out(AbsF32Signal::new(), abs_f32_signal::OUT_OUT, 4, |_| {});
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn rectifies_held_value() {
        let out = value_emits(AbsF32Value::new(), |d| {
            d.set(abs_f32_value::IN_X, -3.0);
        });
        assert_eq!(out, vec![3.0]);
    }
}
