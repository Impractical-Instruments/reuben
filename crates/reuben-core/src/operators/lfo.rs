//! LFO — sine low-frequency modulation source.
//!
//! A control-rate sine oscillator emitting an absolute Signal `out = center + depth *
//! sin(2π·phase)`. It free-runs on the deterministic sample timeline, advancing a phase by
//! `rate / sample_rate` cycles per sample, so the modulation is continuous across blocks /
//! block-slices and never drifts (phase held in f64 like the Clock). Designed to drive
//! another operator's Signal input — e.g. an oscillator's `freq` — for a vibrato/siren drone.
//!
//! - input 0: `rate` (`Float`, Hz) — modulation frequency, read block-rate via `io.value`.
//! - input 1: `depth` (`Float`) — modulation amplitude (added to / subtracted from `center`).
//! - input 2: `center` (`Float`) — bias / offset the modulation swings around.
//! - output 0: `out` (`Float`) — `center + depth * sin(2π·phase)`.
//!
//! `rate`/`depth`/`center` are `Float` inputs (ADR-0028): read block-rate, so a `/lfo/rate`
//! change takes effect at the exact sample of the change (the engine block-slices) and the
//! phase stays continuous across the cut — and any of them can now be *wired* and modulated.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0028): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Lfo {
    inputs:  { rate:   float { 0.01..=20.0,        default 5.0,   "Hz", exp },
               depth:  float { 0.0..=1000.0,       default 10.0,  "",   lin },
               center: float { -1000.0..=20_000.0, default 440.0, "",   lin } },
    outputs: { out: float },
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
            io.value(IN_RATE).max(0.0) as f64 / sample_rate as f64
        } else {
            0.0
        };
        let depth = io.value(IN_DEPTH);
        let center = io.value(IN_CENTER);

        let mut phase = self.phase;
        let out = io.output(OUT_OUT);
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
    use crate::operator::Io;

    const SR: f32 = 48_000.0;

    /// Run `lfo` over one block of `n` frames at the given params, returning the out buffer.
    fn run(lfo: &mut Lfo, n: usize, rate: f32, depth: f32, center: f32) -> Vec<f32> {
        let mut out = vec![0.0f32; n];
        // rate/depth/center are `Float` inputs now (ADR-0028) — supply the per-sample buffers the
        // engine would materialize, in port order (rate, depth, center).
        let rate_buf = vec![rate; n];
        let depth_buf = vec![depth; n];
        let center_buf = vec![center; n];
        {
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let inputs: Vec<Option<&[f32]>> = vec![
                Some(&rate_buf[..]),
                Some(&depth_buf[..]),
                Some(&center_buf[..]),
            ];
            let params: [f32; 0] = [];
            let mut io = Io::new(SR, n, inputs, outs, &params, &[]);
            lfo.process(&mut io);
        }
        out
    }

    #[test]
    fn output_swings_within_bounds_and_means_center() {
        // 5 Hz @ 48 kHz over many whole cycles -> swings in [center-depth, center+depth] and
        // averages to center.
        let center = 440.0;
        let depth = 10.0;
        let mut lfo = Lfo::new();
        // 48000 samples at 5 Hz = exactly 5 whole cycles, so the mean is exactly center.
        let out = run(&mut lfo, 48_000, 5.0, depth, center);

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
        let mut lfo = Lfo::new();
        let out = run(&mut lfo, 48_000, rate, 10.0, center);

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
        // One whole block must equal two back-to-back half-blocks sharing the instance.
        let n = 1000;
        let mut whole = Lfo::new();
        let w = run(&mut whole, 2 * n, 5.0, 10.0, 440.0);

        let mut split = Lfo::new();
        let a = run(&mut split, n, 5.0, 10.0, 440.0);
        let b = run(&mut split, n, 5.0, 10.0, 440.0);

        for i in 0..n {
            assert!((a[i] - w[i]).abs() < 1e-4, "block 1 differs at {i}");
            assert!((b[i] - w[n + i]).abs() < 1e-4, "block 2 differs at {i}");
        }
    }

    #[test]
    fn spawned_lfo_starts_fresh_at_phase_zero() {
        let mut a = Lfo::new();
        let _ = run(&mut a, 5_000, 5.0, 10.0, 440.0);
        let mut b = a.spawn();
        // A fresh spawn starts at phase 0: sin(0) == 0, so the first sample is exactly center.
        let mut out = [0.0f32; 1];
        {
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let (rate, depth, center) = ([5.0f32], [10.0f32], [440.0f32]);
            let inputs: Vec<Option<&[f32]>> =
                vec![Some(&rate[..]), Some(&depth[..]), Some(&center[..])];
            let params: [f32; 0] = [];
            let mut io = Io::new(SR, 1, inputs, outs, &params, &[]);
            b.process(&mut io);
        }
        assert!(
            (out[0] - 440.0).abs() < 1e-4,
            "spawned lfo should start fresh at phase 0 (== center), got {}",
            out[0]
        );
    }
}
