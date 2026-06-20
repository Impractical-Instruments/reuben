//! Filter — state-variable filter, low-pass output.
//!
//! Ports/params are FROZEN (Stage A). DSP body is filled test-first in Stage B.
//!
//! - input 0: `audio` (Signal)
//! - output 0: `audio` (Signal) — low-pass output.
//! - param 0: `cutoff` (Hz)
//! - param 1: `resonance` (0..1)

use crate::descriptor::{Curve, Descriptor, ParamMeta, Port};
use crate::operator::{Io, Operator};

pub const IN_AUDIO: usize = 0;
pub const OUT_AUDIO: usize = 0;
pub const P_CUTOFF: usize = 0;
pub const P_RESONANCE: usize = 1;

#[derive(Default)]
pub struct Filter {
    /// SVF integrator state 1 (continuous across calls / block slices).
    ic1eq: f32,
    /// SVF integrator state 2 (continuous across calls / block slices).
    ic2eq: f32,
}

impl Filter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Filter {
    fn descriptor() -> Descriptor {
        Descriptor {
            type_name: "filter",
            inputs: vec![Port::signal("audio")],
            outputs: vec![Port::signal("audio")],
            params: vec![
                ParamMeta {
                    name: "cutoff",
                    min: 20.0,
                    max: 20_000.0,
                    default: 1_000.0,
                    unit: "Hz",
                    curve: Curve::Exponential,
                },
                ParamMeta {
                    name: "resonance",
                    min: 0.0,
                    max: 1.0,
                    default: 0.2,
                    unit: "",
                    curve: Curve::Linear,
                },
            ],
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Cutoff clamped to a safe range so `tan` never blows up.
        let cutoff = io.param(P_CUTOFF).clamp(20.0, 0.45 * sample_rate);
        // Resonance -> damping k = 1/Q. k = 2 means no resonance (Q = 0.5);
        // smaller k means higher resonance. Clamp away from 0 to stay stable.
        let resonance = io.param(P_RESONANCE).clamp(0.0, 1.0);
        let k = (2.0 - 1.9 * resonance).max(0.1);

        // TPT / zero-delay-feedback bilinear prewarp.
        let g = (std::f32::consts::PI * cutoff / sample_rate).tan();
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;

        for i in 0..n {
            let x = io.input(IN_AUDIO).map(|s| s[i]).unwrap_or(0.0);

            // Andrew Simper / Cytomic SVF update.
            let v3 = x - self.ic2eq;
            let v1 = a1 * self.ic1eq + a2 * v3;
            let v2 = self.ic2eq + a2 * self.ic1eq + a3 * v3;
            self.ic1eq = 2.0 * v1 - self.ic1eq;
            self.ic2eq = 2.0 * v2 - self.ic2eq;

            // Low-pass output.
            io.output(OUT_AUDIO)[i] = v2;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::Io;

    /// Run `input` through a fresh Filter at the given cutoff/resonance and
    /// return the output buffer.
    fn render(input: &[f32], sample_rate: f32, cutoff: f32, resonance: f32) -> Vec<f32> {
        let n = input.len();
        let mut filter = Filter::new();
        let mut out_buf = vec![0.0f32; n];

        let inputs: Vec<Option<&[f32]>> = vec![Some(input)];
        let mut outputs: Vec<&mut [f32]> = vec![out_buf.as_mut_slice()];
        let params = [cutoff, resonance];
        let messages = [];

        let mut io = Io::new(sample_rate, n, &inputs, &mut outputs, &params, &messages);
        filter.process(&mut io);
        out_buf
    }

    /// Generate a pure sine of frequency `f` Hz for `n` samples.
    fn sine(f: f32, sample_rate: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * f * i as f32 / sample_rate).sin())
            .collect()
    }

    fn rms(buf: &[f32]) -> f32 {
        let sum: f32 = buf.iter().map(|x| x * x).sum();
        (sum / buf.len() as f32).sqrt()
    }

    #[test]
    fn filter_attenuates_high_more_than_low() {
        let sr = 48_000.0;
        let n = 8192;
        // Skip the transient at the front to measure steady-state attenuation.
        let warmup = 2048;

        let low = render(&sine(200.0, sr, n), sr, 1_000.0, 0.0);
        let high = render(&sine(8_000.0, sr, n), sr, 1_000.0, 0.0);

        let low_rms = rms(&low[warmup..]);
        let high_rms = rms(&high[warmup..]);

        // Cutoff is 1 kHz: 200 Hz passes, 8 kHz is well below the cutoff.
        assert!(
            low_rms > high_rms * 4.0,
            "expected low ({low_rms}) >> high ({high_rms})"
        );
    }

    #[test]
    fn filter_passes_dc_near_unity() {
        let sr = 48_000.0;
        let n = 4096;
        let input = vec![1.0f32; n];
        let out = render(&input, sr, 1_000.0, 0.0);

        // After settling, a low-pass should pass DC at unity gain.
        let tail = &out[n - 256..];
        let avg = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(
            (avg - 1.0).abs() < 0.01,
            "expected DC near unity, got {avg}"
        );
    }

    #[test]
    fn filter_high_resonance_stays_bounded() {
        let sr = 48_000.0;
        let n = 8192;
        // Drive at the resonant frequency with maximum resonance.
        let input = sine(1_000.0, sr, n);
        let out = render(&input, sr, 1_000.0, 1.0);

        for (i, &s) in out.iter().enumerate() {
            assert!(s.is_finite(), "sample {i} not finite: {s}");
            assert!(s.abs() < 1_000.0, "sample {i} unbounded: {s}");
        }
    }

    #[test]
    fn filter_state_continuous_across_block_slices() {
        // Processing one buffer in one call must equal processing it in two
        // calls that share the same Filter instance.
        let sr = 48_000.0;
        let n = 512;
        let input = sine(440.0, sr, n);

        let whole = render(&input, sr, 1_000.0, 0.3);

        let mut filter = Filter::new();
        let mut out_buf = vec![0.0f32; n];
        let params = [1_000.0f32, 0.3];
        let messages = [];
        let half = n / 2;
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input[..half])];
            let mut outputs: Vec<&mut [f32]> = vec![&mut out_buf[..half]];
            let mut io = Io::new(sr, half, &inputs, &mut outputs, &params, &messages);
            filter.process(&mut io);
        }
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input[half..])];
            let mut outputs: Vec<&mut [f32]> = vec![&mut out_buf[half..]];
            let mut io = Io::new(sr, n - half, &inputs, &mut outputs, &params, &messages);
            filter.process(&mut io);
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
