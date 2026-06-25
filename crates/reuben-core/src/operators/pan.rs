//! Pan — equal-power stereo positioner (ADR-0026).
//!
//! Splits one mono Signal into a left / right pair, placing it in the stereo field with an
//! **equal-power** law (constant perceived loudness across the sweep; −3 dB at center). The
//! two outputs are meant to be tapped directly as `channel: 0` / `channel: 1` of the logical
//! master bus — the `output` op is vestigial for a stereo patch.
//!
//! Pan amount follows the one-port-one-type rule (ADR-0017): it is a **Signal input** with a
//! param as its unwired default, so an LFO can auto-pan, or a static knob can park it.
//!
//! - input 0: `audio` (Signal) — the mono source.
//! - input 1: `pan` (Signal, optional) — position in [-1, 1]; overrides the param.
//! - output 0: `left` (Signal) — `audio · cos(θ)`.
//! - output 1: `right` (Signal) — `audio · sin(θ)`, where `θ = (pan + 1)·π/4`.
//! - param 0: `pan` — used when the `pan` input is unconnected. −1 hard-left, 0 center, +1 hard-right.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0028): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Pan {
    inputs:  { audio: buffer,
               pan:   float { -1.0..=1.0, default 0.0, "", lin } },
    outputs: { left: buffer, right: buffer },
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
            // `audio`/`pan` are `Float` inputs — always a buffer (wired source or materialized
            // latch), one read path (ADR-0028). Read both into locals first so each immutable
            // borrow of `io` ends before the two output writes — keeps `process` allocation-free.
            let a = io.signal(IN_AUDIO).get(i).copied().unwrap_or(0.0);
            let p = io.signal(IN_PAN).get(i).copied().unwrap_or(0.0);
            // Equal-power law: map [-1, 1] -> [0, π/2], split with cos/sin. cos²+sin²=1 keeps
            // total power constant across the sweep; center (p=0) is cos(π/4)=sin(π/4)≈0.707.
            let theta = (p.clamp(-1.0, 1.0) + 1.0) * (core::f32::consts::FRAC_PI_4);
            let l = a * theta.cos();
            let r = a * theta.sin();
            io.signal_mut(OUT_LEFT)[i] = l;
            io.signal_mut(OUT_RIGHT)[i] = r;
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
    use crate::operator::Io;

    const SR: f32 = 48_000.0;

    /// Run pan over one block: `audio` constant 1.0, position from a constant `pan` buffer —
    /// `pan` is a `Float` input now (ADR-0028), supplied as the per-sample buffer the engine
    /// would materialize from the input's latched default.
    fn run_param(pan: f32, n: usize) -> (Vec<f32>, Vec<f32>) {
        let audio = vec![1.0f32; n];
        let pan_buf = vec![pan; n];
        let mut left = vec![0.0f32; n];
        let mut right = vec![0.0f32; n];
        {
            let outs: Vec<&mut [f32]> = vec![&mut left[..], &mut right[..]];
            let inputs: Vec<Option<&[f32]>> = vec![Some(&audio[..]), Some(&pan_buf[..])];
            let mut io = Io::new(SR, n, inputs, outs);
            Pan::new().process(&mut io);
        }
        (left, right)
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
        // A wired `pan` buffer says hard-left — the single read path (ADR-0028) follows it.
        let n = 8;
        let audio = vec![1.0f32; n];
        let pan_in = vec![-1.0f32; n];
        let mut left = vec![0.0f32; n];
        let mut right = vec![0.0f32; n];
        {
            let outs: Vec<&mut [f32]> = vec![&mut left[..], &mut right[..]];
            let inputs: Vec<Option<&[f32]>> = vec![Some(&audio[..]), Some(&pan_in[..])];
            let mut io = Io::new(SR, n, inputs, outs);
            Pan::new().process(&mut io);
        }
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
