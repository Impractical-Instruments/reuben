//! LFO — sine low-frequency modulation source.
//!
//! A control-rate sine oscillator emitting an absolute Signal `out = center + depth *
//! sin(2π·phase)`. It free-runs on the deterministic sample timeline, advancing a phase by
//! `rate / sample_rate` cycles per sample, so the modulation is continuous across blocks /
//! block-slices and never drifts (phase held in f64 like the Clock). Designed to drive
//! another operator's Signal input — e.g. an oscillator's `freq` — for a vibrato/siren drone.
//!
//! `rate`/`depth`/`center` are Value inputs (ADR-0031): read held (block-sliced at changes), so a
//! `/lfo/rate` change takes effect at the exact sample of the change and the phase stays continuous
//! across the cut.
//!
//! - input 0: `rate` (Hz) — modulation frequency.
//! - input 1: `depth` — modulation amplitude (added to / subtracted from `center`).
//! - input 2: `center` — bias / offset the modulation swings around.
//! - output 0: `out` (`Buffer`) — `center + depth * sin(2π·phase)`.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0030): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Lfo {
    inputs:  { rate:   f32 { 0.01..=20.0,        default 5.0,   "Hz", exp },
               depth:  f32 { 0.0..=1000.0,       default 10.0,  "",   lin },
               center: f32 { -1000.0..=20_000.0, default 440.0, "",   lin } },
    outputs: { out: f32_buffer },
});

#[derive(Default)]
pub struct Lfo {
    /// Phase in [0, 1), advanced per sample. Continuous across blocks / slices.
    /// Held in f64 so the modulation grid doesn't drift off the sample timeline over a long
    /// session (f32 accumulation slips audibly within seconds).
    phase: f64,
}

impl Lfo {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Lfo {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Cycles advanced per sample. Rate is constant for this (sub)block (block-sliced).
        let dt: f64 = if sample_rate > 0.0 {
            io.input::<f32>(IN_RATE).unwrap_or(0.0).max(0.0) as f64 / sample_rate as f64
        } else {
            0.0
        };
        let depth = io.input::<f32>(IN_DEPTH).unwrap_or(0.0);
        let center = io.input::<f32>(IN_CENTER).unwrap_or(0.0);

        let mut phase = self.phase;
        let out = io.output::<&mut [f32]>(OUT_OUT);
        for s in out.iter_mut().take(n) {
            let s_val = (std::f64::consts::TAU * phase).sin() as f32;
            *s = center + depth * s_val;
            phase += dt;
            phase -= phase.floor();
        }
        self.phase = phase;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Lfo);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Drive `lfo` for `n` frames at the given params through the real engine, returning the out
    /// buffer. `rate`/`depth`/`center` are held `Float` controls — set once (sticky), read via
    /// `io.input::<f32>`; `out` is the operator's Buffer output, accumulated across the real 128-frame blocks.
    fn run(n: usize, rate: f32, depth: f32, center: f32) -> Vec<f32> {
        OpDriver::for_type(Lfo::new(), SR)
            .set(IN_RATE, rate)
            .set(IN_DEPTH, depth)
            .set(IN_CENTER, center)
            .render(n)
            .output(OUT_OUT)
            .to_vec()
    }

    #[test]
    fn output_swings_within_bounds_and_means_center() {
        // 5 Hz @ 48 kHz over many whole cycles -> swings in [center-depth, center+depth] and
        // averages to center.
        let center = 440.0;
        let depth = 10.0;
        // 48000 samples at 5 Hz = exactly 5 whole cycles, so the mean is exactly center.
        let out = run(48_000, 5.0, depth, center);

        for (i, &s) in out.iter().enumerate() {
            assert!(s.is_finite(), "sample {i} not finite: {s}");
            assert!(
                s >= center - depth - 1e-3 && s <= center + depth + 1e-3,
                "sample {i} out of bounds: {s}"
            );
        }
        let mean = out.iter().sum::<f32>() / out.len() as f32;
        assert!(
            (mean - center).abs() < 0.05,
            "mean {mean} should be ~{center}"
        );
    }

    #[test]
    fn period_matches_sample_rate_over_rate() {
        // 5 Hz @ 48 kHz -> 9600 samples per cycle. Count rising crossings about `center`.
        let center = 440.0;
        let rate = 5.0;
        let out = run(48_000, rate, 10.0, center);

        let mut crossings = Vec::new();
        let mut prev = out[0];
        for (i, &s) in out.iter().enumerate().skip(1) {
            if prev <= center && s > center {
                crossings.push(i);
            }
            prev = s;
        }
        assert!(
            crossings.len() >= 2,
            "expected several cycles, got {crossings:?}"
        );
        let period = crossings[1] - crossings[0];
        let expected = (SR / rate) as usize; // 9600
        assert!(
            period.abs_diff(expected) <= 1,
            "expected ~{expected}-sample period, got {period}"
        );
    }

    #[test]
    fn phase_is_continuous_across_calls() {
        // One render of 2n must equal two back-to-back renders of n sharing the driver's operator
        // (the phase threads across the real 128-frame blocks and across separate `render` calls).
        let n = 1000;
        let w = run(2 * n, 5.0, 10.0, 440.0);

        let mut split = OpDriver::for_type(Lfo::new(), SR);
        split
            .set(IN_RATE, 5.0)
            .set(IN_DEPTH, 10.0)
            .set(IN_CENTER, 440.0);
        let a = split.render(n).output(OUT_OUT).to_vec();
        let b = split.render(n).output(OUT_OUT).to_vec();

        for i in 0..n {
            assert!((a[i] - w[i]).abs() < 1e-4, "block 1 differs at {i}");
            assert!((b[i] - w[n + i]).abs() < 1e-4, "block 2 differs at {i}");
        }
    }

    #[test]
    fn spawned_lfo_starts_fresh_at_phase_zero() {
        let mut a = OpDriver::for_type(Lfo::new(), SR);
        a.set(IN_RATE, 5.0)
            .set(IN_DEPTH, 10.0)
            .set(IN_CENTER, 440.0);
        a.render(5_000);
        // A fresh spawn starts at phase 0: sin(0) == 0, so the first sample is exactly center.
        let mut b = a.spawn();
        let out = b
            .set(IN_RATE, 5.0)
            .set(IN_DEPTH, 10.0)
            .set(IN_CENTER, 440.0)
            .render(1)
            .output(OUT_OUT)
            .to_vec();
        assert!(
            (out[0] - 440.0).abs() < 1e-4,
            "spawned lfo should start fresh at phase 0 (== center), got {}",
            out[0]
        );
    }
}
