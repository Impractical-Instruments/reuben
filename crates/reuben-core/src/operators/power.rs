//! Power — a unipolar curve shaper, `out = x^exponent`, per sample (ADR-0027).
//!
//! The first member of the curve-op family (issue #40): named for the precise math curve it
//! applies (a *power* curve), not a generic "curve" knob — future shapes get their own ops
//! (`logarithmic`, …). It exists to turn the envelope's **linear** CV into a perceptually
//! natural **volume** contour: the ear hears loudness roughly logarithmically, so a linear
//! amplitude decay sounds abrupt. Raising the linear `[0, 1]` contour to a power (≈2) tracks an
//! exponential-style decay closely while still hitting exactly 0 at release and 1 at the peak —
//! no silence floor to fudge. Patch it between an `envelope` and a `mul`: `env.cv -> power.x`,
//! `power.out -> mul`, audio -> the other `mul` input.
//!
//! Shape model (ADR-0028/0029): both inputs are materialized **`Float`** inputs owning their
//! unwired defaults. `exponent` is read once **block-rate** via `io.value` (the curve shape is held
//! for the call, not swept per sample). `x` is read per-sample. Uniform with the rest of the math
//! family — no bare ports, no param slot (ADR-0029). The curve-op precedent: future shapes
//! (`logarithmic`, …) follow this exact shape (dense `Float`, a block-rate shaping operand, op-local
//! guards).
//!
//! - input 0: `x` (`Float`) — the value to shape; treated as unipolar (negatives clamp to 0 so a
//!   fractional exponent never yields NaN). Unwired default 0.
//! - input 1: `exponent` (`Float`) — the power. Default 2 (a musical amplitude curve); 1 is a
//!   pass-through.
//! - output 0: `out` (`Float`) — `x^exponent`.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0028): one declaration -> IN_/OUT_ consts + Descriptor. Both
// inputs are materialized `Float`s with declared defaults (ADR-0029); `x` defaults to 0.
crate::operator_contract!(Power {
    inputs:  { x:        float { -1_000_000.0..=1_000_000.0, default 0.0, "", lin },
               exponent: float { 0.0..=8.0,                  default 2.0, "", lin } },
    outputs: { out: float },
});

/// The op's scalar math, written once (ADR-0029 pure-fn seam): a unipolar power curve. The
/// `max(0.0)` is `power`'s **op-local** NaN guard (a fractional exponent over a negative base is
/// NaN); it lives here, inherited by no other op.
#[inline]
fn shape(x: f32, exponent: f32) -> f32 {
    x.max(0.0).powf(exponent)
}

#[derive(Default)]
pub struct Power;

impl Power {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for Power {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        // `exponent` is a materialized `Float`; read it once block-rate — the curve shape is held
        // for the call, not swept per sample (ADR-0029).
        let exponent = io.value(IN_EXPONENT);
        for i in 0..n {
            // `x` is a materialized `Float` (always a buffer in production); `unwrap_or(0.0)` is the
            // declared default for the empty-slice (unwired) case. The clamp lives in `shape`.
            let x = io.signal(IN_X).get(i).copied().unwrap_or(0.0);
            io.output(OUT_OUT)[i] = shape(x, exponent);
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Power);

#[cfg(test)]
mod tests {
    use super::*;
    use approx::assert_abs_diff_eq;

    const SR: f32 = 48_000.0;

    /// Run `power` over one block with the given input and exponent; returns `out`. `x` and
    /// `exponent` are `Float` inputs now (ADR-0028), supplied as the per-sample buffers the engine
    /// would materialize (port order: x, exponent).
    fn run(x: Option<&[f32]>, exponent: f32) -> Vec<f32> {
        let n = x.map_or(4, <[f32]>::len);
        let mut out = vec![0.0f32; n];
        let exp_buf = vec![exponent; n];
        let params: [f32; 0] = [];
        {
            let inputs: Vec<Option<&[f32]>> = vec![x, Some(&exp_buf[..])];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, n, inputs, outs, &params, &[]);
            Power::new().process(&mut io);
        }
        out
    }

    #[test]
    fn squares_the_input_by_default() {
        let x = [0.0, 0.25, 0.5, 1.0];
        let out = run(Some(&x), 2.0);
        assert_abs_diff_eq!(out[0], 0.0, epsilon = 1e-6);
        assert_abs_diff_eq!(out[1], 0.0625, epsilon = 1e-6);
        assert_abs_diff_eq!(out[2], 0.25, epsilon = 1e-6);
        assert_abs_diff_eq!(out[3], 1.0, epsilon = 1e-6);
    }

    #[test]
    fn fixes_the_endpoints() {
        // Any exponent maps 0 -> 0 and 1 -> 1, so the curve only bends the interior — a
        // release still reaches true silence and a peak still reaches unity.
        for &k in &[0.5, 1.0, 2.0, 3.5, 8.0] {
            let out = run(Some(&[0.0, 1.0]), k);
            assert_abs_diff_eq!(out[0], 0.0, epsilon = 1e-6);
            assert_abs_diff_eq!(out[1], 1.0, epsilon = 1e-6);
        }
    }

    #[test]
    fn exponent_one_is_passthrough() {
        let x = [0.1, 0.3, 0.7, 0.9];
        let out = run(Some(&x), 1.0);
        for (o, i) in out.iter().zip(&x) {
            assert_abs_diff_eq!(o, i, epsilon = 1e-6);
        }
    }

    #[test]
    fn negative_input_clamps_to_zero_no_nan() {
        // A fractional exponent over a negative base would be NaN; the unipolar clamp prevents it.
        let out = run(Some(&[-1.0, -0.5, 0.0]), 0.5);
        assert!(out.iter().all(|s| s.is_finite()));
        assert_abs_diff_eq!(out[0], 0.0, epsilon = 1e-6);
        assert_abs_diff_eq!(out[1], 0.0, epsilon = 1e-6);
    }

    #[test]
    fn unwired_input_is_silent() {
        let out = run(None, 2.0);
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn operand_defaults_are_data() {
        // ADR-0029: both inputs are settable Floats; x defaults to 0, exponent to 2.
        let d = Power::descriptor();
        let default = |name: &str| {
            d.settable_inputs()
                .find(|(n, _)| *n == name)
                .unwrap_or_else(|| panic!("{name} is a settable Float"))
                .1
                .default
        };
        assert_eq!(default("x"), 0.0);
        assert_eq!(default("exponent"), 2.0);
    }

    #[test]
    fn spawned_copy_behaves_identically() {
        let x = [0.2, 0.6, 1.0];
        let direct = run(Some(&x), 3.0);
        let mut out = vec![0.0f32; x.len()];
        let exp_buf = vec![3.0f32; x.len()];
        {
            let spawned = Power::new().spawn();
            let inputs: Vec<Option<&[f32]>> = vec![Some(&x), Some(&exp_buf[..])];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, x.len(), inputs, outs, &[], &[]);
            let mut op = spawned;
            op.process(&mut io);
        }
        assert_eq!(out, direct);
    }
}
