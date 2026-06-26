//! Delay — feedback echo with a dry/wet mix.
//!
//! Port types (ADR-0030): `time`, `feedback`, and `mix` are **`F32` inputs**, each owning its
//! unwired default. When nothing is wired the engine materializes the input from its latched
//! default; when an LFO or envelope is wired the source buffer passes through. They are read
//! block-rate via `io.last::<f32>` (the held ZOH value), and `io.signal(IN_AUDIO)` is always a
//! buffer (wired source or materialized latch).
//!
//! - input 0: `audio` (`Float`) — the signal to delay.
//! - input 1: `time` (`Float`, s) — delay time (materialized default 0.3).
//! - input 2: `feedback` (`Float`) — feedback amount 0..0.95 (materialized default 0.4).
//! - input 3: `mix` (`Float`) — dry/wet blend 0..1 (materialized default 0.5).
//! - output 0: `audio` (`Float`) — the dry+wet mix.
//!
//! DSP: a ring buffer sized to the maximum delay (2 s) is allocated lazily on the first
//! `process` call (mirrors the Voicer idiom — sample_rate isn't known in `new()`). Per
//! sample we read the delayed sample `time*sample_rate` behind the write head with linear
//! interpolation, write `input + feedback*delayed` at the head, and output the dry/wet
//! mix. The ring buffer and head index are continuous across calls / block slices, and
//! `process` allocates nothing in steady state.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0028): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Delay {
    inputs:  { audio: buffer,
               time:     float { 0.001..=2.0, default 0.3, "s", lin },
               feedback: float { 0.0..=0.95,  default 0.4, "",  lin },
               mix:      float { 0.0..=1.0,   default 0.5, "",  lin } },
    outputs: { audio: buffer },
});

/// Maximum delay time in seconds; sizes the ring buffer.
const MAX_DELAY_SECS: f32 = 2.0;

#[derive(Default)]
pub struct Delay {
    /// Ring buffer of past (input + feedback) samples. Allocated lazily on first
    /// `process`, sized to `ceil(MAX_DELAY_SECS * sample_rate)`. Continuous across calls.
    buf: Vec<f32>,
    /// Write head index into `buf` (continuous across calls / block slices).
    head: usize,
}

impl Delay {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Delay {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Lazily size the ring buffer to the max delay (sample_rate isn't known in `new`).
        let cap = (MAX_DELAY_SECS * sample_rate).ceil() as usize;
        let cap = cap.max(1);
        if self.buf.len() != cap {
            self.buf = vec![0.0f32; cap];
            self.head = 0;
        }

        let feedback = io.last::<f32>(IN_FEEDBACK).unwrap_or(0.0).clamp(0.0, 0.95);
        let mix = io.last::<f32>(IN_MIX).unwrap_or(0.0).clamp(0.0, 1.0);
        // Read offset in samples; clamp so the interpolated tap stays inside the buffer.
        let time = io
            .last::<f32>(IN_TIME)
            .unwrap_or(0.0)
            .clamp(0.001, MAX_DELAY_SECS);
        let delay_samples = (time * sample_rate).clamp(1.0, (cap - 1) as f32);

        let len = self.buf.len();
        for i in 0..n {
            let x = io.signal(IN_AUDIO).get(i).copied().unwrap_or(0.0);

            // Fractional read position `delay_samples` behind the write head.
            let read_pos = self.head as f32 + len as f32 - delay_samples;
            let base = read_pos.floor() as usize;
            let frac = read_pos - read_pos.floor();
            let i0 = base % len;
            let i1 = (base + 1) % len;
            let delayed = self.buf[i0] * (1.0 - frac) + self.buf[i1] * frac;

            // Feed input + feedback of the delayed signal into the line.
            self.buf[self.head] = x + feedback * delayed;

            // Dry/wet mix.
            io.signal_mut(OUT_AUDIO)[i] = (1.0 - mix) * x + mix * delayed;

            self.head = (self.head + 1) % len;
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Delay);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    /// Drive `input` through a fresh Delay at the given values through the real engine, returning the
    /// output buffer. `time`/`feedback`/`mix` are held `Float` controls (`set` once, read via
    /// `io.last`); `audio` is a time-varying Buffer input (`drive`d block by block). The state threads
    /// across the real 128-frame blocks, so an echo lands at its true sample offset across them.
    fn render(input: &[f32], sample_rate: f32, time: f32, feedback: f32, mix: f32) -> Vec<f32> {
        OpDriver::for_type(Delay::new(), sample_rate)
            .set(IN_TIME, time)
            .set(IN_FEEDBACK, feedback)
            .set(IN_MIX, mix)
            .drive(IN_AUDIO, input)
            .render(input.len())
            .output(OUT_AUDIO)
            .to_vec()
    }

    /// Index of the largest-magnitude sample in `buf`.
    fn argmax_abs(buf: &[f32]) -> usize {
        buf.iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.abs().partial_cmp(&b.abs()).unwrap())
            .map(|(i, _)| i)
            .unwrap()
    }

    #[test]
    fn impulse_produces_a_delayed_echo() {
        let sr = 48_000.0;
        let time = 0.1; // 4800 samples
        let n = 12_000;
        // A single unit impulse at frame 0.
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;

        // mix = 1 (fully wet) so the echo is isolated from the dry impulse.
        let out = render(&input, sr, time, 0.0, 1.0);

        let expected = (time * sr) as usize; // 4800
                                             // The first wet echo should land at ~time seconds.
        let peak = argmax_abs(&out[1..]) + 1;
        assert!(
            (expected as i64 - peak as i64).abs() <= 1,
            "echo expected near {expected}, peaked at {peak}"
        );
        assert!(out[peak] > 0.9, "echo too quiet: {}", out[peak]);
    }

    #[test]
    fn feedback_produces_multiple_decaying_echoes() {
        let sr = 48_000.0;
        let time = 0.05; // 2400 samples
        let n = 12_000;
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;

        let fb = 0.5;
        let out = render(&input, sr, time, fb, 1.0);

        let step = (time * sr) as usize; // 2400
                                         // Successive echoes appear at multiples of the delay and decay by `feedback`.
        let e1 = out[step];
        let e2 = out[2 * step];
        let e3 = out[3 * step];
        assert!(e1 > 0.4 && e1 < 1.1, "first echo {e1}");
        assert!(
            e2 > 0.0 && e2 < e1,
            "second echo {e2} should be < first {e1}"
        );
        assert!(
            e3 > 0.0 && e3 < e2,
            "third echo {e3} should be < second {e2}"
        );
        // Decay ratio tracks the feedback coefficient.
        assert!(
            (e2 / e1 - fb).abs() < 0.05,
            "decay ratio {} != {fb}",
            e2 / e1
        );
    }

    #[test]
    fn mix_zero_is_dry_passthrough() {
        let sr = 48_000.0;
        let n = 2048;
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr).sin())
            .collect();
        // Even with heavy feedback, mix = 0 must pass the dry input through untouched.
        let out = render(&input, sr, 0.3, 0.9, 0.0);
        for i in 0..n {
            assert!(
                (out[i] - input[i]).abs() < 1e-6,
                "mix=0 not dry at {i}: {} vs {}",
                out[i],
                input[i]
            );
        }
    }

    #[test]
    fn high_feedback_stays_bounded() {
        let sr = 48_000.0;
        let n = 48_000;
        // Sustained noise-ish input driving the line at maximum feedback.
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 220.0 * i as f32 / sr).sin())
            .collect();
        let out = render(&input, sr, 0.05, 0.95, 0.7);
        for (i, &s) in out.iter().enumerate() {
            assert!(s.is_finite(), "sample {i} not finite: {s}");
            assert!(s.abs() < 100.0, "sample {i} unbounded: {s}");
        }
    }

    // The former `state_continuous_across_block_slices` (a hand-built two-`Io`-call split) is
    // retired: `OpDriver::render` always steps the operator as real 128-frame blocks, so every test
    // here crosses dozens of block boundaries. `impulse_produces_a_delayed_echo` (echo at frame
    // 4800, ~37 blocks in) and `feedback_produces_multiple_decaying_echoes` already prove the ring
    // buffer + head index thread continuously across them — there is no longer a "whole vs split"
    // path to compare, the engine owns the slicing.
}
