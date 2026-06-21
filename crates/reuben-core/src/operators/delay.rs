//! Delay — feedback echo with a dry/wet mix.
//!
//! Ports/params are FROZEN (Stage A). DSP body is filled test-first in Stage B.
//!
//! - input 0: `audio` (Signal)
//! - output 0: `audio` (Signal) — the dry+wet mix.
//! - param 0: `time` (s) — delay time.
//! - param 1: `feedback` (0..0.95)
//! - param 2: `mix` (0..1) — dry/wet blend.
//!
//! DSP: a ring buffer sized to the maximum delay (2 s) is allocated lazily on the first
//! `process` call (mirrors the Voicer idiom — sample_rate isn't known in `new()`). Per
//! sample we read the delayed sample `time*sample_rate` behind the write head with linear
//! interpolation, write `input + feedback*delayed` at the head, and output the dry/wet
//! mix. The ring buffer and head index are continuous across calls / block slices, and
//! `process` allocates nothing in steady state.

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::operator::{Io, Operator};

pub const IN_AUDIO: usize = 0;
pub const OUT_AUDIO: usize = 0;
pub const P_TIME: usize = 0;
pub const P_FEEDBACK: usize = 1;
pub const P_MIX: usize = 2;

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
        Descriptor {
            type_name: "delay",
            inputs: vec![Port::signal("audio")],
            outputs: vec![Port::signal("audio")],
            params: vec![
                ParamMeta {
                    name: "time",
                    min: 0.001,
                    max: 2.0,
                    default: 0.3,
                    unit: "s",
                    curve: Curve::Linear,
                },
                ParamMeta {
                    name: "feedback",
                    min: 0.0,
                    max: 0.95,
                    default: 0.4,
                    unit: "",
                    curve: Curve::Linear,
                },
                ParamMeta {
                    name: "mix",
                    min: 0.0,
                    max: 1.0,
                    default: 0.5,
                    unit: "",
                    curve: Curve::Linear,
                },
            ],
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
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

        let feedback = io.param(P_FEEDBACK).clamp(0.0, 0.95);
        let mix = io.param(P_MIX).clamp(0.0, 1.0);
        // Read offset in samples; clamp so the interpolated tap stays inside the buffer.
        let time = io.param(P_TIME).clamp(0.001, MAX_DELAY_SECS);
        let delay_samples = (time * sample_rate).clamp(1.0, (cap - 1) as f32);

        let len = self.buf.len();
        for i in 0..n {
            let x = io.input(IN_AUDIO).map(|s| s[i]).unwrap_or(0.0);

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
            io.output(OUT_AUDIO)[i] = (1.0 - mix) * x + mix * delayed;

            self.head = (self.head + 1) % len;
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::Io;

    /// Run `input` through a fresh Delay at the given params and return the output buffer.
    fn render(input: &[f32], sample_rate: f32, time: f32, feedback: f32, mix: f32) -> Vec<f32> {
        let n = input.len();
        let mut delay = Delay::new();
        let mut out_buf = vec![0.0f32; n];

        let params = [time, feedback, mix];
        let messages = [];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(input)];
            let outputs: Vec<&mut [f32]> = vec![out_buf.as_mut_slice()];
            let mut io = Io::new(sample_rate, n, inputs, outputs, &params, &messages);
            delay.process(&mut io);
        }
        out_buf
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

    #[test]
    fn state_continuous_across_block_slices() {
        // One call must equal two back-to-back calls sharing the same Delay instance.
        let sr = 48_000.0;
        let n = 4096;
        let time = 0.02;
        let fb = 0.6;
        let mix = 0.5;
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 330.0 * i as f32 / sr).sin())
            .collect();

        let whole = render(&input, sr, time, fb, mix);

        let mut delay = Delay::new();
        let mut out_buf = vec![0.0f32; n];
        let params = [time, fb, mix];
        let messages = [];
        let half = n / 2;
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input[..half])];
            let outputs: Vec<&mut [f32]> = vec![&mut out_buf[..half]];
            let mut io = Io::new(sr, half, inputs, outputs, &params, &messages);
            delay.process(&mut io);
        }
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input[half..])];
            let outputs: Vec<&mut [f32]> = vec![&mut out_buf[half..]];
            let mut io = Io::new(sr, n - half, inputs, outputs, &params, &messages);
            delay.process(&mut io);
        }

        for i in 0..n {
            assert!(
                (whole[i] - out_buf[i]).abs() < 1e-5,
                "slice mismatch at {i}: {} vs {}",
                whole[i],
                out_buf[i]
            );
        }
    }
}
