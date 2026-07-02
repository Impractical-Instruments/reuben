//! `max` — `out = max(a, b)`, per sample (ADR-0029, ADR-0017, ADR-0033).
//!
//! The sanctioned way to **take the higher of two streams**: a floor on CV, the lower half of a
//! clamp, half-wave rectification (`max(x, 0)`). A dense `Float`→`Float` op whose arithmetic is the
//! f32 [`max_fn`], called once per sample by the signal shell and once per change by the value shell
//! (issue #83).
//!
//! Unlike add/mul there is no finite identity, so `b`'s unwired default is the **range minimum**
//! (`-1e6`): `max(a, -1e6) == a` for any in-range signal, so wiring only `a` passes it through. The
//! comparison needs only `PartialOrd`, so [`max_fn`] is generic over the number type (ADR-0029
//! pure-fn seam).
//!
//! - input 0: `a` (`Float`) — first operand. Unwired default `0`.
//! - input 1: `b` (`Float`) — second operand. Unwired default `-1e6` (the range min — a no-op).
//! - output 0: `out` — `max(a, b)`.

/// The op's scalar math, written once (ADR-0029 pure-fn seam) and generic over any `PartialOrd`
/// number: the greater of the two operands (ties return `a`). Hand-written rather than [`Ord::max`]
/// so it covers `f32`, which is only `PartialOrd`.
#[inline]
fn max_fn<T: PartialOrd>(a: T, b: T) -> T {
    if b > a {
        b
    } else {
        a
    }
}

// One declaration -> MaxF32Value + MaxF32Signal (ADR-0033). `b` defaults to the range minimum so an
// unwired second operand is a no-op (`max(a, -1e6) == a`), passing `a` through (ADR-0029).
crate::number_operator_contract!(Max {
    numbers:  [f32],
    carriers: [value, signal],
    inputs:   { a: number { default 0.0 }, b: number { default min } },
    outputs:  { out },
    function: max_fn(a, b),
});

#[cfg(test)]
mod tests {
    use super::max_f32_signal::{self, MaxF32Signal};
    use super::max_f32_value::{self, MaxF32Value};
    use crate::operators::math_test::{signal_out, value_emits};

    /// Drive the signal form; `None` leaves the port unwired (engine materializes its default).
    fn sig(a: Option<&[f32]>, b: Option<&[f32]>, n: usize) -> Vec<f32> {
        signal_out(MaxF32Signal::new(), max_f32_signal::OUT_OUT, n, |d| {
            if let Some(a) = a {
                d.drive(max_f32_signal::IN_A, a);
            }
            if let Some(b) = b {
                d.drive(max_f32_signal::IN_B, b);
            }
        })
    }

    /// Drive the value form; returns the emitted maximum(s).
    fn val(a: f32, b: f32) -> Vec<f32> {
        value_emits(MaxF32Value::new(), |d| {
            d.set(max_f32_value::IN_A, a);
            d.set(max_f32_value::IN_B, b);
        })
    }

    #[test]
    fn takes_higher_of_two_buffers() {
        let a = [1.0, 5.0, 3.0];
        let b = [4.0, 2.0, 3.0];
        assert_eq!(sig(Some(&a), Some(&b), 3), vec![4.0, 5.0, 3.0]);
    }

    #[test]
    fn unwired_b_passes_a_through() {
        // b defaults to the range min, a no-op for in-range signals.
        let a = [-5.0, 0.0, 7.0];
        assert_eq!(sig(Some(&a), None, 3), vec![-5.0, 0.0, 7.0]);
    }

    #[test]
    fn half_wave_rectifies_against_zero() {
        // max(x, 0) keeps the positive half, floors the negative half.
        let x = [-1.0, -0.5, 0.5, 1.0];
        let z = [0.0; 4];
        assert_eq!(sig(Some(&x), Some(&z), 4), vec![0.0, 0.0, 0.5, 1.0]);
    }

    #[test]
    fn takes_higher_of_held_operands() {
        assert_eq!(val(3.0, 8.0), vec![8.0]);
        assert_eq!(val(8.0, 3.0), vec![8.0]);
    }
}
