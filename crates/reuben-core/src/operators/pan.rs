//! Pan — equal-power stereo positioner (ADR-0026).
//!
//! Splits one mono Signal into a left / right pair, placing it in the stereo field with an
//! **equal-power** law (constant perceived loudness across the sweep; −3 dB at center). The
//! two outputs are meant to be tapped directly as `channel: 0` / `channel: 1` of the logical
//! master bus — the `output` op is vestigial for a stereo patch.
//!
//! Pan amount is a **Signal input** with a default as its unwired value (ADR-0031), so an LFO can
//! auto-pan or a static knob can park it.
//!
//! - input 0: `audio` (Signal) — the mono source.
//! - input 1: `pan` (Signal) — position in [-1, 1]; −1 hard-left, 0 center, +1 hard-right.
//!   Unwired default 0 (center).
//! - output 0: `left` (Signal) — `audio · cos(θ)`.
//! - output 1: `right` (Signal) — `audio · sin(θ)`, where `θ = (pan + 1)·π/4`.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Pan {
    inputs:  { audio: f32_buffer,
               pan:   f32_buffer { -1.0..=1.0, default 0.0, "", lin } },
    outputs: { left: f32_buffer, right: f32_buffer },
});

#[derive(Default)]
pub struct Pan;

impl Pan {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for Pan {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();

        for i in 0..n {
            // `audio`/`pan` are Signal inputs — always a buffer (wired source or materialized
            // default), one read path (ADR-0031). Read both into locals first so each immutable
            // borrow of `io` ends before the two output writes — keeps `process` allocation-free.
            let a = io.read(IN_AUDIO)[i];
            let p = io.read(IN_PAN)[i];
            // Equal-power law: map [-1, 1] -> [0, π/2], split with cos/sin. cos²+sin²=1 keeps
            // total power constant across the sweep; center (p=0) is cos(π/4)=sin(π/4)≈0.707.
            let theta = (p.clamp(-1.0, 1.0) + 1.0) * (core::f32::consts::FRAC_PI_4);
            let l = a * theta.cos();
            let r = a * theta.sin();
            io.write(OUT_LEFT)[i] = l;
            io.write(OUT_RIGHT)[i] = r;
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Pan);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Run pan over `n` frames through the real engine: `audio` is a constant 1.0 audio-in and
    /// `pan` a held position (both `set`, ZOH-materialized to per-sample buffers). Returns the
    /// (left, right) stereo outputs.
    fn run_param(pan: f32, n: usize) -> (Vec<f32>, Vec<f32>) {
        let mut d = OpDriver::for_type(Pan::new(), SR);
        d.set(IN_AUDIO, 1.0).set(IN_PAN, pan).render(n);
        (d.output(OUT_LEFT).to_vec(), d.output(OUT_RIGHT).to_vec())
    }

    #[test]
    fn center_is_equal_power_minus_3db() {
        // Center: both channels ≈ 1/√2 (−3 dB), and the power splits evenly.
        let (l, r) = run_param(0.0, 16);
        let expect = 1.0 / 2.0f32.sqrt();
        assert!((l[0] - expect).abs() < 1e-5, "left {} != {expect}", l[0]);
        assert!((r[0] - expect).abs() < 1e-5, "right {} != {expect}", r[0]);
        assert!(
            (l[0] * l[0] + r[0] * r[0] - 1.0).abs() < 1e-5,
            "power not unity at center"
        );
    }

    #[test]
    fn hard_left_and_hard_right() {
        let (l, r) = run_param(-1.0, 8);
        assert!(
            (l[0] - 1.0).abs() < 1e-5,
            "hard-left should pass full signal left"
        );
        assert!(r[0].abs() < 1e-5, "hard-left should silence the right");

        let (l, r) = run_param(1.0, 8);
        assert!(l[0].abs() < 1e-5, "hard-right should silence the left");
        assert!(
            (r[0] - 1.0).abs() < 1e-5,
            "hard-right should pass full signal right"
        );
    }

    #[test]
    fn power_is_constant_across_the_sweep() {
        // Equal-power means l²+r² == 1 for any position.
        for &p in &[-1.0, -0.5, -0.1, 0.0, 0.3, 0.7, 1.0] {
            let (l, r) = run_param(p, 4);
            let power = l[0] * l[0] + r[0] * r[0];
            assert!((power - 1.0).abs() < 1e-5, "power {power} != 1 at pan {p}");
        }
    }

    #[test]
    fn pan_input_overrides_param() {
        // A wired `pan` buffer says hard-left — the single read path (ADR-0031) follows it.
        let n = 8;
        let pan_in = vec![-1.0f32; n];
        let mut d = OpDriver::for_type(Pan::new(), SR);
        d.set(IN_AUDIO, 1.0).drive(IN_PAN, &pan_in).render(n);
        let left = d.output(OUT_LEFT);
        let right = d.output(OUT_RIGHT);
        assert!(
            (left[0] - 1.0).abs() < 1e-5,
            "pan input should win (hard-left)"
        );
        assert!(right[0].abs() < 1e-5, "pan input should win (right silent)");
    }

    #[test]
    fn out_of_range_pan_is_clamped() {
        // A pan beyond ±1 (e.g. an over-driven LFO) clamps, not wraps.
        let (l, r) = run_param(5.0, 4);
        assert!(
            l[0].abs() < 1e-5 && (r[0] - 1.0).abs() < 1e-5,
            "should clamp to hard-right"
        );
    }
}
