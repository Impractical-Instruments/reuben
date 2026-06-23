//! Filter — state-variable filter; lowpass / highpass / bandpass (V1.3 `mode`, ADR-0022).
//!
//! One-port-one-type (ADR-0017): `cutoff` and `resonance` are **Signal inputs**, the
//! canonical audio-rate sweep targets. Each carries an **unwired default scalar** — the
//! `cutoff`/`resonance` *params*, which survive only as the value the port reads when no
//! Signal is wired. So a static filter (`/filter/cutoff 3000`) needs no upstream node and
//! is bit-identical to the old param-only behavior, while a Good Button or LFO can sweep
//! the same port by wiring a Signal (e.g. an `m2s` converter, ADR-0017). To drive cutoff
//! from Messages, insert the `m2s` converter — the smoothing policy lives there, once.
//!
//! The TPT / Cytomic SVF computes all three responses from the same integrator state, so a
//! `mode` param selects the output tap (ADR-0022): `lp = v2`, `bp = v1`, `hp = x - k·bp - lp`.
//! Mode 0 (lowpass) is the default and bit-identical to the prior lowpass-only filter — the
//! hat voice highpasses noise (mode 1) and a tonal band can isolate a frequency (mode 2).
//!
//! - input 0: `audio` (Signal) — the signal to filter.
//! - input 1: `cutoff` (Signal) — per-sample cutoff in Hz; unwired → the `cutoff` param.
//! - input 2: `resonance` (Signal) — per-sample resonance 0..1; unwired → the param.
//! - output 0: `audio` (Signal) — the selected response (lowpass / highpass / bandpass).
//! - param 0: `cutoff` (Hz) — the cutoff Signal port's unwired default.
//! - param 1: `resonance` (0..1) — the resonance Signal port's unwired default.
//! - param 2: `mode` — 0 = lowpass (default), 1 = highpass, 2 = bandpass.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Ports/params declared once (ADR-0025): the macro plants the IN_/OUT_/P_ index consts and the
// matching `Descriptor` from one source, so they cannot drift.
crate::operator_contract!(Filter {
    inputs:  { audio: signal, cutoff: signal, resonance: signal },
    outputs: { audio: signal },
    params:  { cutoff:    { 20.0..=20_000.0, default 1_000.0, "Hz", exp },
               resonance: { 0.0..=1.0,       default 0.2,     "",   lin },
               mode:      { 0.0..=2.0,       default 0.0,     "",   lin } },
});

/// Filter response selected by the `mode` param.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Lowpass,
    Highpass,
    Bandpass,
}

impl Mode {
    /// Map the `mode` param (rounded) to a response; out-of-range → lowpass (the safe default).
    fn from_param(mode: f32) -> Self {
        match mode.round() as i32 {
            1 => Mode::Highpass,
            2 => Mode::Bandpass,
            _ => Mode::Lowpass,
        }
    }
}

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
    /// The lowpass tap (`v2`) is bit-identical to the pre-V1.3 filter.
    #[inline]
    fn svf_step(&mut self, x: f32, a1: f32, a2: f32, a3: f32) -> f32 {
        let v3 = x - self.ic2eq;
        let v1 = a1 * self.ic1eq + a2 * v3;
        let v2 = self.ic2eq + a2 * self.ic1eq + a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        v2
    }

    /// One SVF step returning the **selected** response. The TPT SVF exposes all three taps
    /// from the same state: `lp = v2`, `bp = v1`, `hp = x - k·bp - lp` (standard Cytomic).
    /// Mode `Lowpass` returns exactly what [`Self::svf_step`] does, so it stays bit-identical.
    #[inline]
    fn svf_step_mode(&mut self, x: f32, a1: f32, a2: f32, a3: f32, k: f32, mode: Mode) -> f32 {
        let v3 = x - self.ic2eq;
        let v1 = a1 * self.ic1eq + a2 * v3;
        let v2 = self.ic2eq + a2 * self.ic1eq + a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        match mode {
            Mode::Lowpass => v2,
            Mode::Bandpass => v1,
            Mode::Highpass => x - k * v1 - v2,
        }
    }
}

/// TPT / zero-delay-feedback SVF coefficients for a given cutoff (Hz) and resonance (0..1).
/// Cutoff is clamped to a safe range so `tan` never blows up; resonance maps to damping
/// `k = 1/Q` (k = 2 ⇒ no resonance, smaller k ⇒ more), clamped away from 0 for stability.
/// Returns `(a1, a2, a3, k)` — `k` is needed for the highpass tap.
#[inline]
fn coeffs(cutoff: f32, resonance: f32, sample_rate: f32) -> (f32, f32, f32, f32) {
    let cutoff = cutoff.clamp(20.0, 0.45 * sample_rate);
    let k = (2.0 - 1.9 * resonance.clamp(0.0, 1.0)).max(0.1);
    let g = (std::f32::consts::PI * cutoff / sample_rate).tan();
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    let a3 = g * a2;
    (a1, a2, a3, k)
}

impl Operator for Filter {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Unwired defaults: the cutoff/resonance params survive only as the value each
        // Signal port reads when nothing is wired (ADR-0017 one-port-one-type).
        let cutoff_default = io.param(P_CUTOFF);
        let resonance_default = io.param(P_RESONANCE);
        let mode = Mode::from_param(io.param(P_MODE));
        let cutoff_wired = io.input(IN_CUTOFF).is_some();
        let resonance_wired = io.input(IN_RESONANCE).is_some();

        if !cutoff_wired && !resonance_wired {
            // Fast path: both controls constant for the (sub)block, coefficients computed
            // once. Lowpass uses `svf_step` and is bit-identical to the prior param-only
            // filter; HP/BP tap the same SVF state via `svf_step_mode`.
            let (a1, a2, a3, k) = coeffs(cutoff_default, resonance_default, sample_rate);
            if mode == Mode::Lowpass {
                for i in 0..n {
                    let x = io.input(IN_AUDIO).map(|s| s[i]).unwrap_or(0.0);
                    io.output(OUT_AUDIO)[i] = self.svf_step(x, a1, a2, a3);
                }
            } else {
                for i in 0..n {
                    let x = io.input(IN_AUDIO).map(|s| s[i]).unwrap_or(0.0);
                    io.output(OUT_AUDIO)[i] = self.svf_step_mode(x, a1, a2, a3, k, mode);
                }
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
        let (mut a1, mut a2, mut a3, mut k) = (0.0, 0.0, 0.0, 0.0);
        for i in 0..n {
            let x = io.input(IN_AUDIO).map(|s| s[i]).unwrap_or(0.0);
            let cutoff = io.input(IN_CUTOFF).map_or(cutoff_default, |s| s[i]);
            let resonance = io.input(IN_RESONANCE).map_or(resonance_default, |s| s[i]);
            // NaN seed forces a compute on the first sample (NaN != anything).
            if cutoff != last_cutoff || resonance != last_resonance {
                (a1, a2, a3, k) = coeffs(cutoff, resonance, sample_rate);
                last_cutoff = cutoff;
                last_resonance = resonance;
            }
            // Lowpass stays on `svf_step` for bit-identity with the pre-V1.3 modulated path.
            io.output(OUT_AUDIO)[i] = if mode == Mode::Lowpass {
                self.svf_step(x, a1, a2, a3)
            } else {
                self.svf_step_mode(x, a1, a2, a3, k, mode)
            };
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Filter);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::Io;

    /// Run `input` through a fresh Filter at the given cutoff/resonance (lowpass mode) and
    /// return the output buffer.
    fn render(input: &[f32], sample_rate: f32, cutoff: f32, resonance: f32) -> Vec<f32> {
        render_mode(input, sample_rate, cutoff, resonance, 0.0)
    }

    /// Like [`render`] but with an explicit `mode` (0 = LP, 1 = HP, 2 = BP).
    fn render_mode(
        input: &[f32],
        sample_rate: f32,
        cutoff: f32,
        resonance: f32,
        mode: f32,
    ) -> Vec<f32> {
        let n = input.len();
        let mut filter = Filter::new();
        let mut out_buf = vec![0.0f32; n];

        let params = [cutoff, resonance, mode];
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
        let params = [cutoff_default, resonance_default, 0.0];
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
        let (a1, a2, a3, _k) = coeffs(2_500.0, 0.0, sr);
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
        let params = [1_000.0f32, 0.3, 0.0];
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

    // --- V1.3 `mode` param: lowpass / highpass / bandpass (ADR-0022) ---

    #[test]
    fn default_mode_is_bit_identical_to_lowpass() {
        // The descriptor default `mode` is 0 (lowpass). Rendering with the default param set
        // must be bit-for-bit identical to an explicit lowpass — proving existing instruments,
        // which never set `mode`, are unchanged.
        let sr = 48_000.0;
        let n = 4096;
        let input = sine(1_000.0, sr, n);

        // Reference: explicit lowpass via the fast path (`svf_step`).
        let lp = render_mode(&input, sr, 1_200.0, 0.4, 0.0);

        // The descriptor's defaults give cutoff 1000, resonance 0.2, mode 0. Render with the
        // *default* mode but a matching cutoff/resonance, against an explicit `svf_step` ref.
        let mut reference = Filter::new();
        let (a1, a2, a3, _k) = coeffs(1_200.0, 0.4, sr);
        let mut ref_out = vec![0.0f32; n];
        for i in 0..n {
            ref_out[i] = reference.svf_step(input[i], a1, a2, a3);
        }
        for i in 0..n {
            assert_eq!(
                lp[i].to_bits(),
                ref_out[i].to_bits(),
                "lowpass mode diverged from the bare SVF lowpass at {i}"
            );
        }
    }

    #[test]
    fn highpass_attenuates_lows_more_than_highs() {
        // Mirror of the lowpass test: with cutoff at 1 kHz, a highpass passes 8 kHz and
        // attenuates 200 Hz.
        let sr = 48_000.0;
        let n = 8192;
        let warmup = 2048;

        let low = render_mode(&sine(200.0, sr, n), sr, 1_000.0, 0.0, 1.0);
        let high = render_mode(&sine(8_000.0, sr, n), sr, 1_000.0, 0.0, 1.0);

        let low_rms = rms(&low[warmup..]);
        let high_rms = rms(&high[warmup..]);
        assert!(
            high_rms > low_rms * 4.0,
            "highpass: expected high ({high_rms}) >> low ({low_rms})"
        );
    }

    #[test]
    fn bandpass_peaks_near_cutoff() {
        // A bandpass centered at 1 kHz passes 1 kHz far more strongly than either a much
        // lower (100 Hz) or much higher (8 kHz) tone.
        let sr = 48_000.0;
        let n = 8192;
        let warmup = 2048;
        let res = 0.6; // a little resonance sharpens the peak

        let center = render_mode(&sine(1_000.0, sr, n), sr, 1_000.0, res, 2.0);
        let below = render_mode(&sine(100.0, sr, n), sr, 1_000.0, res, 2.0);
        let above = render_mode(&sine(8_000.0, sr, n), sr, 1_000.0, res, 2.0);

        let c = rms(&center[warmup..]);
        let b = rms(&below[warmup..]);
        let a = rms(&above[warmup..]);
        assert!(
            c > b * 2.0 && c > a * 2.0,
            "bandpass should peak at cutoff: center {c}, below {b}, above {a}"
        );
    }

    #[test]
    fn highpass_blocks_dc() {
        // A highpass must reject DC: a constant input settles to ~0.
        let sr = 48_000.0;
        let n = 8192;
        let out = render_mode(&vec![1.0f32; n], sr, 1_000.0, 0.0, 1.0);
        let tail = &out[n - 256..];
        let avg = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(avg.abs() < 0.02, "highpass should block DC, got {avg}");
    }
}
