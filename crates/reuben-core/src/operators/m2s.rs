//! `m2s` — the gap-filling Message→Signal shaper (ADR-0017, ADR-0030).
//!
//! Control is Message-first; a dense CV is the opt-in special case. In the unified model the wire
//! **already** materializes an `F32` source into a `Buffer` by zero-order hold (the implicit ZOH
//! bridge, ADR-0030) — that is the old `Snap` mode, now automatic. So `m2s` exists only for the
//! gap-*filling* policies the plain step can't express: how to move *between* the held target's
//! changes. That policy is `mode`, and it lives **here, once** (the reason cutoff/freq/etc. are
//! Buffer-only — never re-implemented per operator).
//!
//! - input 0: `in` (`Float`, held) — the target value. Block-sliced at each change, so a mid-block
//!   retarget is sample-accurate; the unwired default is the resting value (a Good Button tone at
//!   load).
//! - input 1: `mode` (`Enum` [`M2sMode`] {Smooth, Slew, Glide}) — the fill policy; default `Smooth`.
//! - input 2: `rate` (`Float`) — slew rate in units/second.
//! - input 3: `time` (`Float`) — time constant in seconds (smooth) or ramp time (glide).
//! - output 0: `out` (`Buffer`) — the materialized per-sample control signal.
//!
//! Modes:
//! - **smooth** — one-pole exponential approach (`time`); the natural knob feel.
//! - **slew** — rate-limited linear approach (`rate` units/s).
//! - **glide** — fixed-time linear ramp to the target (`time`); portamento, retargeting on change.
//!
//! True linear interpolation *between* targets is excluded — it needs the next value, so it is not
//! RT-causal without a one-block delay (ADR-0017). State (current value, glide ramp) carries across
//! blocks. The target is read **per-sample** from `in`'s buffer (an `F32` control always presents
//! one — the materialized ZOH of a held/sparse target, or a wired CV source), so a stepped source
//! retargets exactly at its change frame and a continuous source is tracked sample-accurately.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::M2sMode;

// Single-source contract (ADR-0025/0030). `mode` references the shared `M2sMode` vocab enum, whose
// `#[default]` is `Smooth` — the natural knob feel and the prior default.
crate::operator_contract!(M2s {
    inputs:  { in:   float { -1_000_000.0..=1_000_000.0, default 0.0,     "",   lin },
               mode: enum(M2sMode),
               rate: float { 0.0..=1_000_000.0,          default 1_000.0, "/s", exp },
               time: float { 0.0..=10.0,                 default 0.05,    "s",  exp } },
    outputs: { out: buffer },
});

#[derive(Default)]
pub struct M2s {
    /// Current output value, held across blocks.
    cur: f32,
    /// Target the current value is approaching (the last held `in`).
    target: f32,
    /// Glide ramp: per-sample increment and remaining samples.
    glide_inc: f32,
    glide_left: u32,
    /// Whether `cur`/`target` have been seeded from the first target yet.
    initialized: bool,
}

impl M2s {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for M2s {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sr = io.sample_rate();
        let mode = io.last::<M2sMode>(IN_MODE).unwrap_or_default();
        let rate = io.last::<f32>(IN_RATE).unwrap_or(0.0).max(0.0);
        let time = io.last::<f32>(IN_TIME).unwrap_or(0.0).max(0.0);

        // Per-sample smoothing coefficients.
        let tau_samples = (time * sr).max(1e-6);
        let smooth_coeff = 1.0 - (-1.0 / tau_samples).exp();
        let slew_step = if sr > 0.0 { rate / sr } else { 0.0 };
        let glide_total = (time * sr).round().max(1.0);

        let mut cur = self.cur;
        let mut target = self.target;
        let mut glide_inc = self.glide_inc;
        let mut glide_left = self.glide_left;
        let mut initialized = self.initialized;

        for i in 0..n {
            // `in` is an F32 control, so it always presents a buffer (ADR-0030): the materialized
            // ZOH of a held/sparse target, or a wired CV source. Read it per-sample so a continuous
            // source is tracked and a stepped (sparse-message) source retargets exactly at its
            // change frame. The immutable read is copied out before the mutable output write below.
            let t = io.signal(IN_IN).get(i).copied().unwrap_or(0.0);
            if !initialized {
                cur = t;
                target = t;
                initialized = true;
            }
            // A retarget. Glide re-arms its fixed-time ramp from the current value toward the new
            // target (a stepped source fires this only at its change frame).
            if t != target {
                target = t;
                if mode == M2sMode::Glide {
                    glide_inc = (target - cur) / glide_total;
                    glide_left = glide_total as u32;
                }
            }
            match mode {
                M2sMode::Slew => {
                    if cur < target {
                        cur = (cur + slew_step).min(target);
                    } else {
                        cur = (cur - slew_step).max(target);
                    }
                }
                M2sMode::Smooth => cur += (target - cur) * smooth_coeff,
                M2sMode::Glide => {
                    if glide_left > 0 {
                        cur += glide_inc;
                        glide_left -= 1;
                    } else {
                        cur = target;
                    }
                }
            }
            io.signal_mut(OUT_OUT)[i] = cur;
        }

        self.cur = cur;
        self.target = target;
        self.glide_inc = glide_inc;
        self.glide_left = glide_left;
        self.initialized = initialized;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(M2s);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Arg;

    const SR: f32 = 48_000.0;

    /// Run one block: `mode` is the held `M2sMode`, `rate`/`time`/`in` the held `Float` inputs the
    /// engine latches; returns `out`. `target` is the held `in` value for the whole call (a mid-
    /// block retarget would be a separate sliced call).
    fn run(
        mode: M2sMode,
        rate: f32,
        time: f32,
        target: f32,
        n: usize,
        state: &mut M2s,
    ) -> Vec<f32> {
        let latched = [
            Arg::F32(target),
            Arg::M2sMode(mode),
            Arg::F32(rate),
            Arg::F32(time),
        ];
        // `in` is read per-sample from its buffer (ADR-0030); a constant block = a held target.
        let in_buf = vec![target; n];
        let mut out = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&in_buf[..]), None, None, None];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, n, inputs, outs).with_latched(&latched);
            state.process(&mut io);
        }
        out
    }

    #[test]
    fn resting_value_held_before_movement() {
        // Smooth with target == start: the whole block sits at the resting value.
        let out = run(M2sMode::Smooth, 1000.0, 0.05, 4000.0, 64, &mut M2s::new());
        assert!(out.iter().all(|&s| (s - 4000.0).abs() < 1e-3));
    }

    #[test]
    fn slew_is_rate_limited() {
        // rate = 48000 units/s @ 48k => 1.0 unit/sample. Seed resting at 0, then retarget to 10:
        // it slews 0 -> 10, reaching in 10 samples.
        let mut m = M2s::new();
        let _ = run(M2sMode::Slew, 48_000.0, 0.05, 0.0, 1, &mut m); // seed resting value
        let out = run(M2sMode::Slew, 48_000.0, 0.05, 10.0, 64, &mut m);
        approx::assert_relative_eq!(out[0], 1.0, epsilon = 1e-4);
        approx::assert_relative_eq!(out[9], 10.0, epsilon = 1e-4);
        assert!(out[20..].iter().all(|&s| (s - 10.0).abs() < 1e-4));
    }

    #[test]
    fn smooth_approaches_monotonically_without_overshoot() {
        let out = run(M2sMode::Smooth, 1000.0, 0.01, 1.0, 2048, &mut M2s::new());
        for w in out.windows(2) {
            assert!(w[1] >= w[0] - 1e-6, "smooth must not decrease");
            assert!(w[1] <= 1.0 + 1e-6, "smooth must not overshoot");
        }
        assert!(out[out.len() - 1] > 0.9, "should approach the target");
    }

    #[test]
    fn glide_ramps_linearly_over_time() {
        // glide, time = 64/48000 s => exactly 64-sample ramp. Seed resting at 0, then retarget to
        // 64: it ramps 0 -> 64 over 64 samples.
        let time = 64.0 / SR;
        let mut m = M2s::new();
        let _ = run(M2sMode::Glide, 1000.0, time, 0.0, 1, &mut m); // seed resting value
        let out = run(M2sMode::Glide, 1000.0, time, 64.0, 128, &mut m);
        approx::assert_relative_eq!(out[31], 32.0, epsilon = 1.5);
        assert!(out[64..].iter().all(|&s| (s - 64.0).abs() < 1e-3));
    }

    #[test]
    fn value_carries_across_blocks() {
        // smooth: the partially-approached value at block end resumes next block.
        let mut m = M2s::new();
        let _ = run(M2sMode::Smooth, 1000.0, 0.05, 0.0, 1, &mut m); // seed resting at 0
        let b1 = run(M2sMode::Smooth, 1000.0, 0.05, 1.0, 64, &mut m);
        let b2 = run(M2sMode::Smooth, 1000.0, 0.05, 1.0, 64, &mut m);
        assert!(
            b2[0] >= b1[63] - 1e-6,
            "must continue approaching, not reset"
        );
        assert!(b2[63] > b1[63], "keeps rising across the boundary");
    }

    #[test]
    fn spawned_converter_starts_uninitialized() {
        let mut m = M2s::new();
        let _ = run(M2sMode::Slew, 1000.0, 0.05, 1.0, 64, &mut m);
        let mut m2 = m.spawn();
        // Fresh spawn re-seeds from its first target (7.0), not where `m` ended (1.0).
        let latched = [
            Arg::F32(7.0),
            Arg::M2sMode(M2sMode::Slew),
            Arg::F32(1000.0),
            Arg::F32(0.05),
        ];
        let in_buf = [7.0f32; 8];
        let mut out = [0.0f32; 8];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&in_buf[..]), None, None, None];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, 8, inputs, outs).with_latched(&latched);
            m2.process(&mut io);
        }
        assert!(out.iter().all(|&s| (s - 7.0).abs() < 1e-3));
    }
}
