//! Filter — state-variable filter, low-pass output.
//!
//! One-port-one-type (ADR-0017): `cutoff` and `resonance` are **Signal inputs**, the
//! canonical audio-rate sweep targets. Each carries an **unwired default scalar** — the
//! `cutoff`/`resonance` *params*, which survive only as the value the port reads when no
//! Signal is wired. So a static filter (`/filter/cutoff 3000`) needs no upstream node and
//! is bit-identical to the old param-only behavior, while a Good Button or LFO can sweep
//! the same port by wiring a Signal (e.g. an `m2s` converter, ADR-0017). To drive cutoff
//! from Messages, insert the `m2s` converter — the smoothing policy lives there, once.
//!
//! - input 0: `audio` (Signal) — the signal to filter.
//! - input 1: `cutoff` (Signal) — per-sample cutoff in Hz; unwired → the `cutoff` param.
//! - input 2: `resonance` (Signal) — per-sample resonance 0..1; unwired → the param.
//! - output 0: `audio` (Signal) — low-pass output.
//! - param 0: `cutoff` (Hz) — the cutoff Signal port's unwired default.
//! - param 1: `resonance` (0..1) — the resonance Signal port's unwired default.

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::operator::{Io, Operator};

pub const IN_AUDIO: usize = 0;
pub const IN_CUTOFF: usize = 1;
pub const IN_RESONANCE: usize = 2;
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

    /// One Cytomic SVF sample step against precomputed coefficients; returns the low-pass
    /// output and advances the integrator state. Shared by the constant and modulated paths.
    #[inline]
    fn svf_step(&mut self, x: f32, a1: f32, a2: f32, a3: f32) -> f32 {
        let v3 = x - self.ic2eq;
        let v1 = a1 * self.ic1eq + a2 * v3;
        let v2 = self.ic2eq + a2 * self.ic1eq + a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        v2
    }
}

/// TPT / zero-delay-feedback SVF coefficients for a given cutoff (Hz) and resonance (0..1).
/// Cutoff is clamped to a safe range so `tan` never blows up; resonance maps to damping
/// `k = 1/Q` (k = 2 ⇒ no resonance, smaller k ⇒ more), clamped away from 0 for stability.
#[inline]
fn coeffs(cutoff: f32, resonance: f32, sample_rate: f32) -> (f32, f32, f32) {
    let cutoff = cutoff.clamp(20.0, 0.45 * sample_rate);
    let k = (2.0 - 1.9 * resonance.clamp(0.0, 1.0)).max(0.1);
    let g = (std::f32::consts::PI * cutoff / sample_rate).tan();
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    let a3 = g * a2;
    (a1, a2, a3)
}

impl Operator for Filter {
    fn descriptor() -> Descriptor {
        Descriptor {
            type_name: "filter",
            inputs: vec![
                Port::signal("audio"),
                Port::signal("cutoff"),
                Port::signal("resonance"),
            ],
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
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Unwired defaults: the cutoff/resonance params survive only as the value each
        // Signal port reads when nothing is wired (ADR-0017 one-port-one-type).
        let cutoff_default = io.param(P_CUTOFF);
        let resonance_default = io.param(P_RESONANCE);
        let cutoff_wired = io.input(IN_CUTOFF).is_some();
        let resonance_wired = io.input(IN_RESONANCE).is_some();

        if !cutoff_wired && !resonance_wired {
            // Fast path: both controls constant for the (sub)block, coefficients computed
            // once. Bit-identical to the prior param-only filter.
            let (a1, a2, a3) = coeffs(cutoff_default, resonance_default, sample_rate);
            for i in 0..n {
                let x = io.input(IN_AUDIO).map(|s| s[i]).unwrap_or(0.0);
                let v2 = self.svf_step(x, a1, a2, a3);
                io.output(OUT_AUDIO)[i] = v2;
            }
            return;
        }

        // Modulated path: at least one of cutoff/resonance is a wired Signal. Read each per
        // sample (audio-rate sweep), falling back to its param default when unwired.
        // Coefficients are recomputed only when the (cutoff, resonance) pair actually changes
        // from the previous sample, so a settled or slowly-moving control costs one compare
        // per sample instead of a `tan()`. `coeffs` is pure, so reusing the cached triple on
        // an unchanged input is bit-identical to recomputing it every sample. A genuinely
        // audio-rate sweep still recomputes per sample; a coarser control-rate recompute for
        // that case is tracked in #24.
        let mut last_cutoff = f32::NAN;
        let mut last_resonance = f32::NAN;
        let (mut a1, mut a2, mut a3) = (0.0, 0.0, 0.0);
        for i in 0..n {
            let x = io.input(IN_AUDIO).map(|s| s[i]).unwrap_or(0.0);
            let cutoff = io.input(IN_CUTOFF).map_or(cutoff_default, |s| s[i]);
            let resonance = io.input(IN_RESONANCE).map_or(resonance_default, |s| s[i]);
            // NaN seed forces a compute on the first sample (NaN != anything).
            if cutoff != last_cutoff || resonance != last_resonance {
                (a1, a2, a3) = coeffs(cutoff, resonance, sample_rate);
                last_cutoff = cutoff;
                last_resonance = resonance;
            }
            let v2 = self.svf_step(x, a1, a2, a3);
            io.output(OUT_AUDIO)[i] = v2;
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

    /// Run `input` through a fresh Filter at the given cutoff/resonance and
    /// return the output buffer.
    fn render(input: &[f32], sample_rate: f32, cutoff: f32, resonance: f32) -> Vec<f32> {
        let n = input.len();
        let mut filter = Filter::new();
        let mut out_buf = vec![0.0f32; n];

        let params = [cutoff, resonance];
        let messages = [];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(input)];
            let outputs: Vec<&mut [f32]> = vec![out_buf.as_mut_slice()];
            let mut io = Io::new(sample_rate, n, inputs, outputs, &params, &messages);
            filter.process(&mut io);
        }
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

    /// Run `input` with explicit per-sample cutoff/resonance Signal inputs.
    fn render_modulated(
        input: &[f32],
        sample_rate: f32,
        cutoff: Option<&[f32]>,
        resonance: Option<&[f32]>,
        cutoff_default: f32,
        resonance_default: f32,
    ) -> Vec<f32> {
        let n = input.len();
        let mut filter = Filter::new();
        let mut out_buf = vec![0.0f32; n];
        let params = [cutoff_default, resonance_default];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(input), cutoff, resonance];
            let outputs: Vec<&mut [f32]> = vec![out_buf.as_mut_slice()];
            let mut io = Io::new(sample_rate, n, inputs, outputs, &params, &[]);
            filter.process(&mut io);
        }
        out_buf
    }

    #[test]
    fn wired_cutoff_input_matches_equivalent_param() {
        // A constant cutoff Signal must produce exactly the same output as the same value
        // set as the param (the unwired default) — the modulated path with a flat control
        // equals the fast path.
        let sr = 48_000.0;
        let n = 4096;
        let input = sine(8_000.0, sr, n);
        let via_param = render(&input, sr, 1_000.0, 0.0);
        let cutoff_buf = vec![1_000.0f32; n];
        let via_input = render_modulated(&input, sr, Some(&cutoff_buf), None, 3_000.0, 0.0);
        for i in 0..n {
            assert!(
                (via_param[i] - via_input[i]).abs() < 1e-4,
                "wired cutoff should match param at {i}: {} vs {}",
                via_param[i],
                via_input[i]
            );
        }
    }

    #[test]
    fn sweeping_cutoff_opens_the_filter() {
        // A rising cutoff sweep lets progressively more of a fixed high tone through: the
        // second half (cutoff high) is louder than the first (cutoff low).
        let sr = 48_000.0;
        let n = 8192;
        let input = sine(6_000.0, sr, n);
        let cutoff: Vec<f32> = (0..n)
            .map(|i| 300.0 + (i as f32 / n as f32) * 11_700.0)
            .collect();
        let out = render_modulated(&input, sr, Some(&cutoff), None, 1_000.0, 0.0);
        let first = rms(&out[1024..n / 2]);
        let second = rms(&out[n / 2..]);
        assert!(
            second > first * 2.0,
            "opening the cutoff should pass more signal: first {first}, second {second}"
        );
    }

    #[test]
    fn cached_coeffs_are_bit_identical_to_per_sample_recompute() {
        // The modulated path caches coefficients and recomputes only when (cutoff, resonance)
        // changes. Because `coeffs` is pure, the output must be bit-for-bit identical to
        // recomputing every sample — both for a constant control and a per-sample sweep.
        let sr = 48_000.0;
        let n = 4096;
        let input = sine(6_000.0, sr, n);

        // Constant control: every sample reuses the cache after the first.
        let constant = vec![2_500.0f32; n];
        let out = render_modulated(&input, sr, Some(&constant), None, 1_000.0, 0.0);

        // Reference: a fresh filter stepping the same once-computed coeffs every sample.
        let mut reference = Filter::new();
        let (a1, a2, a3) = coeffs(2_500.0, 0.0, sr);
        let mut ref_out = vec![0.0f32; n];
        for i in 0..n {
            ref_out[i] = reference.svf_step(input[i], a1, a2, a3);
        }
        for i in 0..n {
            assert_eq!(
                out[i].to_bits(),
                ref_out[i].to_bits(),
                "cached constant-control output diverged at {i}"
            );
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
            let outputs: Vec<&mut [f32]> = vec![&mut out_buf[..half]];
            let mut io = Io::new(sr, half, inputs, outputs, &params, &messages);
            filter.process(&mut io);
        }
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input[half..])];
            let outputs: Vec<&mut [f32]> = vec![&mut out_buf[half..]];
            let mut io = Io::new(sr, n - half, inputs, outputs, &params, &messages);
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
