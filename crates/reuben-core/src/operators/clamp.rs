//! `clamp` — `out = clamp(x, lo, hi)`, per sample.
//!
//! The sanctioned way to **bound a stream to a range**: a limiter on CV, a safety rail on a
//! modulated parameter, hard clipping of audio at `±lo/hi`. A dense `Float`→`Float` op whose
//! arithmetic is the f32 [`clamp_fn`], called once per sample by the signal shell and once per change
//! by the value shell (issue #83). The bounds `lo`/`hi` follow the carrier like any number operand,
//! so they can be modulated per sample.
//!
//! **Non-panicking.** [`Ord::clamp`] (and [`f32::clamp`]) panics when `min > max`; since `lo`/`hi`
//! are live inputs that could momentarily cross, [`clamp_fn`] instead composes `max(lo)` then
//! `min(hi)` by hand — an inverted range collapses to `hi` rather than panicking on the hot path. It
//! needs only `PartialOrd`, so it is generic over the number type (the pure-fn seam).
//!
//! - input 0: `x` (`Float`) — the value to bound. Unwired default `0`.
//! - input 1: `lo` (`Float`) — the lower bound. Unwired default `-1`.
//! - input 2: `hi` (`Float`) — the upper bound. Unwired default `1`.
//! - output 0: `out` — `x` bounded to `[lo, hi]`.

/// The op's scalar math, written once (the pure-fn seam) and generic over any `PartialOrd`
/// number. Composed as `max(lo)` then `min(hi)` by hand rather than [`Ord::clamp`] so a transiently
/// inverted `lo > hi` collapses to `hi` instead of **panicking** on the render thread (the
/// hot-path-safety choice). Only `PartialOrd` is required, so it works for any ordered number type.
#[inline]
fn clamp_fn<T: PartialOrd>(x: T, lo: T, hi: T) -> T {
    // max(lo): the larger of x and lo.
    let lower = if lo > x { lo } else { x };
    // min(hi): the smaller of that and hi.
    if hi < lower {
        hi
    } else {
        lower
    }
}

// One declaration -> ClampF32Value + ClampF32Signal. The default range is the bipolar
// audio range [-1, 1], so an unconfigured clamp is a standard hard limiter.
crate::number_operator_contract!(Clamp {
    numbers:  [f32],
    carriers: [value, signal],
    inputs:   { x: number { default 0.0 }, lo: number { default -1.0 }, hi: number { default 1.0 } },
    outputs:  { out },
    function: clamp_fn(x, lo, hi),
});

#[cfg(test)]
mod tests {
    use super::clamp_f32_signal::{self, ClampF32Signal};
    use super::clamp_f32_value::{self, ClampF32Value};
    use crate::operators::math_test::{signal_out, value_emits};

    /// Drive the signal form: `x` is a per-sample buffer; the bounds are held (set once, materialised).
    fn sig(x: &[f32], lo: f32, hi: f32) -> Vec<f32> {
        signal_out(
            ClampF32Signal::new(),
            clamp_f32_signal::OUT_OUT,
            x.len(),
            |d| {
                d.set(clamp_f32_signal::IN_LO, lo);
                d.set(clamp_f32_signal::IN_HI, hi);
                d.drive(clamp_f32_signal::IN_X, x);
            },
        )
    }

    #[test]
    fn passes_in_range_clamps_out_of_range() {
        // [-1, 1]: -2 -> -1, 0.5 stays, 3 -> 1.
        let out = sig(&[-2.0, -0.5, 0.5, 3.0], -1.0, 1.0);
        assert_eq!(out, vec![-1.0, -0.5, 0.5, 1.0]);
    }

    #[test]
    fn respects_custom_bounds() {
        let out = sig(&[0.0, 5.0, 12.0], 2.0, 10.0);
        assert_eq!(out, vec![2.0, 5.0, 10.0]);
    }

    #[test]
    fn inverted_bounds_do_not_panic() {
        // lo > hi must not panic on the hot path (unlike f32::clamp); it collapses to `hi`.
        let out = sig(&[-5.0, 0.0, 5.0], 1.0, -1.0);
        assert!(out.iter().all(|s| s.is_finite()));
        assert_eq!(out, vec![-1.0, -1.0, -1.0]);
    }

    #[test]
    fn default_bounds_are_bipolar_audio_range() {
        // Unwired lo/hi default to [-1, 1].
        let out = signal_out(ClampF32Signal::new(), clamp_f32_signal::OUT_OUT, 3, |d| {
            d.drive(clamp_f32_signal::IN_X, &[-4.0, 0.25, 4.0]);
        });
        assert_eq!(out, vec![-1.0, 0.25, 1.0]);
    }

    #[test]
    fn clamps_held_value() {
        let out = value_emits(ClampF32Value::new(), |d| {
            d.set(clamp_f32_value::IN_X, 9.0);
            d.set(clamp_f32_value::IN_LO, 0.0);
            d.set(clamp_f32_value::IN_HI, 5.0);
        });
        assert_eq!(out, vec![5.0]);
    }
}
