//! PowerF32Signal — a unipolar curve shaper, `out = x^exponent`, per sample (ADR-0027).
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
//! Port types (ADR-0029/0030): both inputs are materialized **`F32`** inputs owning their
//! unwired defaults. `exponent` is read once **block-rate** via `io.last` (the curve shape is held
//! for the call, not swept per sample). `x` is read per-sample. Uniform with the rest of the math
//! family — no bare ports, no param slot (ADR-0029). The curve-op precedent: future shapes
//! (`logarithmic`, …) follow this exact shape (dense `Float`, a block-rate shaping operand, op-local
//! guards).
//!
//! - input 0: `x` (`Float`) — the value to shape; treated as unipolar (negatives clamp to 0 so a
//!   fractional exponent never yields NaN). Unwired default 0.
//! - input 1: `exponent` (`Float`) — the power. Default 2 (a musical amplitude curve); 1 is a
//!   pass-through.
//! - output 0: `out` (`Buffer`) — `x^exponent`.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0030): one declaration -> IN_/OUT_ consts + Descriptor. Both
// inputs are materialized `Float`s with declared defaults (ADR-0029); `x` defaults to 0.
crate::operator_contract!(PowerF32Signal {
    inputs:  { x:        f32_buffer { -1_000_000.0..=1_000_000.0, default 0.0, "", lin },
               exponent: f32 { 0.0..=8.0,                  default 2.0, "", lin } },
    outputs: { out: f32_buffer },
});

/// The op's scalar math, written once (ADR-0029 pure-fn seam): a unipolar power curve. The
/// `max(0.0)` is `power`'s **op-local** NaN guard (a fractional exponent over a negative base is
/// NaN); it lives here, inherited by no other op.
#[inline]
fn shape(x: f32, exponent: f32) -> f32 {
    x.max(0.0).powf(exponent)
}

#[derive(Default)]
pub struct PowerF32Signal;

impl PowerF32Signal {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for PowerF32Signal {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        // `exponent` is a materialized `Float`; read its held (ZOH) value once block-rate — the
        // curve shape is held for the call, not swept per sample (ADR-0029/0030).
        let exponent = io.input::<f32>(IN_EXPONENT).unwrap_or(2.0);
        for i in 0..n {
            // `x` is a materialized `Float` (always a buffer in production); `unwrap_or(0.0)` is the
            // declared default for the empty-slice (unwired) case. The clamp lives in `shape`.
            let x = io.input::<&[f32]>(IN_X).get(i).copied().unwrap_or(0.0);
            io.output::<&mut [f32]>(OUT_OUT)[i] = shape(x, exponent);
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(PowerF32Signal);

/// Value-carrier form of `power` (ADR-0031): both `x` and `exponent` are **held** `f32`, one held
/// output. Reads both once and emits `x^exponent` as a single deduped `MsgWriter` change. Reuses the
/// shared scalar [`shape`] (issue #83 seam, incl. its op-local unipolar NaN guard) — the value shell
/// calls it once where the signal shell loops it. Block-slicing re-runs `process` at every operand
/// change (post-flip, when the ports are Value), so the output is sample-accurate with no buffer. A
/// forced submodule: the contract macro emits its `IN_`/`OUT_` consts at module scope.
pub mod value {
    use super::shape;
    use crate::descriptor::Descriptor;
    use crate::operator::{Io, Operator};

    // Same operands/defaults as the signal form (`x` 0, `exponent` 2) — but `x` is `f32` (held),
    // not a buffer. `exponent` was already block-rate `f32` in the signal form.
    crate::operator_contract!(PowerF32Value {
        inputs:  { x:        f32 { -1_000_000.0..=1_000_000.0, default 0.0, "", lin },
                   exponent: f32 { 0.0..=8.0,                  default 2.0, "", lin } },
        outputs: { out: f32 { -1_000_000.0..=1_000_000.0, default 0.0, "", lin } },
    });

    #[derive(Default)]
    pub struct PowerF32Value;

    impl PowerF32Value {
        pub fn new() -> Self {
            Self
        }
    }

    impl Operator for PowerF32Value {
        fn descriptor() -> Descriptor {
            Self::contract()
        }

        fn process(&mut self, io: &mut Io) {
            // Both operands held; read once. `unwrap_or` supplies the declared defaults.
            let x = io.input::<f32>(IN_X).unwrap_or(0.0);
            let exponent = io.input::<f32>(IN_EXPONENT).unwrap_or(2.0);
            io.output::<f32>(OUT_OUT).set(0, shape(x, exponent));
        }

        fn spawn(&self) -> Box<dyn Operator> {
            Box::new(Self::new())
        }
    }

    crate::register_operator!(PowerF32Value);

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::message::{Arg, Emit};
        use crate::op_driver::OpDriver;
        use approx::assert_abs_diff_eq;

        const SR: f32 = 48_000.0;

        /// The F32 value carried by an emit (panics on any other Arg — the contract is F32).
        fn val(e: &Emit) -> f32 {
            match &e.arg {
                Arg::F32(v) => *v,
                other => panic!("expected an F32 result, got {other:?}"),
            }
        }

        /// Drive `x`/`exponent` as block-rate held constants; returns the emitted shaped value(s).
        fn run(x: f32, exponent: f32) -> Vec<f32> {
            let mut d = OpDriver::for_type(PowerF32Value::new(), SR);
            d.set(IN_X, x).set(IN_EXPONENT, exponent);
            d.render(64).emits().iter().map(val).collect()
        }

        #[test]
        fn squares_the_input_by_default() {
            let out = run(0.5, 2.0);
            assert_eq!(out.len(), 1);
            assert_abs_diff_eq!(out[0], 0.25, epsilon = 1e-6);
        }

        #[test]
        fn exponent_one_is_passthrough() {
            let out = run(0.7, 1.0);
            assert_abs_diff_eq!(out[0], 0.7, epsilon = 1e-6);
        }

        #[test]
        fn negative_input_clamps_to_zero_no_nan() {
            // The shared `shape`'s unipolar clamp prevents a NaN from a fractional exponent.
            let out = run(-0.5, 0.5);
            assert!(out[0].is_finite());
            assert_abs_diff_eq!(out[0], 0.0, epsilon = 1e-6);
        }

        #[test]
        fn operand_defaults_are_data() {
            let d = PowerF32Value::descriptor();
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;
    use approx::assert_abs_diff_eq;

    const SR: f32 = 48_000.0;

    /// Drive `power` through the real engine; returns `out`. `x` is a per-sample `Float` buffer
    /// (`Some` drives it, `None` leaves it unwired so the engine materializes its default `0`);
    /// `exponent` is the held block-rate `Float` (read via `io.last`, so `set` once).
    fn run(x: Option<&[f32]>, exponent: f32) -> Vec<f32> {
        let n = x.map_or(4, <[f32]>::len);
        let mut d = OpDriver::for_type(PowerF32Signal::new(), SR);
        d.set(IN_EXPONENT, exponent);
        if let Some(x) = x {
            d.drive(IN_X, x);
        }
        d.render(n).output(OUT_OUT).to_vec()
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
        let d = PowerF32Signal::descriptor();
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
        // A fresh spawn (PowerF32Signal is stateless) reproduces the direct render exactly.
        let base = OpDriver::for_type(PowerF32Signal::new(), SR);
        let out = base
            .spawn()
            .set(IN_EXPONENT, 3.0)
            .drive(IN_X, &x)
            .render(x.len())
            .output(OUT_OUT)
            .to_vec();
        assert_eq!(out, direct);
    }
}
