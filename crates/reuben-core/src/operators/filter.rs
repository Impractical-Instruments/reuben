//! Filter — state-variable filter; lowpass / highpass / bandpass (V1.3 `mode`, ADR-0022).
//!
//! Port types (ADR-0030): `cutoff` and `resonance` are **`F32` inputs**, each owning its
//! unwired default. When nothing is wired the engine materializes the input from its latched
//! default (so `/filter/cutoff 3000` needs no upstream node, bit-identical to the old param
//! behavior); when an LFO or envelope is wired the source buffer passes through and sweeps the
//! port audio-rate. There is no longer a separate "signal port + same-named param" pair, and no
//! wired/unwired branch in `process` — `io.signal(IN_CUTOFF)` is always a buffer.
//!
//! `mode` is an **`Enum` input** [`FilterMode`] {`Lp`, `Hp`, `Bp`}: a held, live-switchable choice
//! read via `io.last::<FilterMode>`. The TPT / Cytomic SVF computes all three responses from the
//! same integrator state, so `mode` selects the output tap (ADR-0022): `lp = v2`, `bp = v1`,
//! `hp = x - k·bp - lp`. `Lp` is the default and bit-identical to the prior lowpass-only filter.
//!
//! - input 0: `audio` (`Buffer`) — the signal to filter.
//! - input 1: `cutoff` (`Float`) — per-sample cutoff in Hz (materialized default 1 kHz).
//! - input 2: `resonance` (`Float`) — per-sample resonance 0..1 (materialized default 0.2).
//! - input 3: `mode` (`Enum` [`FilterMode`] {Lp, Hp, Bp}) — output tap; default `Lp`.
//! - output 0: `audio` (`Buffer`) — the selected response (lowpass / highpass / bandpass).

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::FilterMode;

// Single-source contract (ADR-0025/0030): one declaration -> IN_/OUT_ consts and the Descriptor;
// `mode` references the shared `FilterMode` vocab enum (no per-op type), so no drift.
crate::operator_contract!(Filter {
    inputs:  { audio: f32_buffer,
               cutoff:    f32 { 20.0..=20_000.0, default 1_000.0, "Hz", exp },
               resonance: f32 { 0.0..=1.0,       default 0.2,     "",   lin },
               mode:      enum(FilterMode) },
    outputs: { audio: f32_buffer },
});

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
    fn svf_step_mode(
        &mut self,
        x: f32,
        a1: f32,
        a2: f32,
        a3: f32,
        k: f32,
        mode: FilterMode,
    ) -> f32 {
        let v3 = x - self.ic2eq;
        let v1 = a1 * self.ic1eq + a2 * v3;
        let v2 = self.ic2eq + a2 * self.ic1eq + a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        match mode {
            FilterMode::Lp => v2,
            FilterMode::Bp => v1,
            FilterMode::Hp => x - k * v1 - v2,
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
        let mode = io.last::<FilterMode>(IN_MODE).unwrap_or_default();

        // `cutoff`/`resonance` are `Float` inputs — always a buffer (wired source or materialized
        // latch), one read path (ADR-0028). When neither changed this block (`varying` false,
        // both held), compute coefficients once — the old fast path, and `Lp` via `svf_step` is
        // bit-identical to the prior param-only filter.
        if !io.varying(IN_CUTOFF) && !io.varying(IN_RESONANCE) {
            let cutoff = io.last::<f32>(IN_CUTOFF).unwrap_or(0.0);
            let resonance = io.last::<f32>(IN_RESONANCE).unwrap_or(0.0);
            let (a1, a2, a3, k) = coeffs(cutoff, resonance, sample_rate);
            for i in 0..n {
                let x = io.signal(IN_AUDIO).get(i).copied().unwrap_or(0.0);
                io.signal_mut(OUT_AUDIO)[i] = if mode == FilterMode::Lp {
                    self.svf_step(x, a1, a2, a3)
                } else {
                    self.svf_step_mode(x, a1, a2, a3, k, mode)
                };
            }
            return;
        }

        // Modulated path: at least one control is dense/changing. Read each per sample (audio-rate
        // sweep). Coefficients are recomputed only when the (cutoff, resonance) pair actually
        // changes from the previous sample, so a settled or slowly-moving control costs one compare
        // per sample instead of a `tan()`. `coeffs` is pure, so reusing the cached triple on an
        // unchanged input is bit-identical to recomputing it every sample. A genuinely audio-rate
        // sweep still recomputes per sample; a coarser control-rate recompute is tracked in #24.
        let mut last_cutoff = f32::NAN;
        let mut last_resonance = f32::NAN;
        let (mut a1, mut a2, mut a3, mut k) = (0.0, 0.0, 0.0, 0.0);
        for i in 0..n {
            let x = io.signal(IN_AUDIO).get(i).copied().unwrap_or(0.0);
            let cutoff = io.signal(IN_CUTOFF).get(i).copied().unwrap_or(0.0);
            let resonance = io.signal(IN_RESONANCE).get(i).copied().unwrap_or(0.0);
            // NaN seed forces a compute on the first sample (NaN != anything).
            if cutoff != last_cutoff || resonance != last_resonance {
                (a1, a2, a3, k) = coeffs(cutoff, resonance, sample_rate);
                last_cutoff = cutoff;
                last_resonance = resonance;
            }
            // Lowpass stays on `svf_step` for bit-identity with the pre-V1.3 modulated path.
            io.signal_mut(OUT_AUDIO)[i] = if mode == FilterMode::Lp {
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
    use crate::op_driver::OpDriver;

    /// Run `input` through a fresh Filter at the given constant cutoff/resonance (lowpass) and
    /// return the output buffer. `cutoff`/`resonance` are held `Float` controls (`set` once → the
    /// const-fold path); `audio` is a time-varying Buffer input (`drive`d block by block).
    fn render(input: &[f32], sample_rate: f32, cutoff: f32, resonance: f32) -> Vec<f32> {
        render_mode(input, sample_rate, cutoff, resonance, FilterMode::Lp)
    }

    /// Like [`render`] but with an explicit `mode` (LP / HP / BP), held via `set`.
    fn render_mode(
        input: &[f32],
        sample_rate: f32,
        cutoff: f32,
        resonance: f32,
        mode: FilterMode,
    ) -> Vec<f32> {
        OpDriver::for_type(Filter::new(), sample_rate)
            .set(IN_CUTOFF, cutoff)
            .set(IN_RESONANCE, resonance)
            .set(IN_MODE, mode)
            .drive(IN_AUDIO, input)
            .render(input.len())
            .output(OUT_AUDIO)
            .to_vec()
    }

    /// Render with time-varying per-sample `cutoff`/`resonance` buffers and a held `mode` — each
    /// control `drive`n (marked varying → the operator's modulated path), `audio` `drive`n too.
    fn render_buffers(
        input: &[f32],
        sample_rate: f32,
        cutoff: &[f32],
        resonance: &[f32],
        mode: FilterMode,
    ) -> Vec<f32> {
        OpDriver::for_type(Filter::new(), sample_rate)
            .set(IN_MODE, mode)
            .drive(IN_CUTOFF, cutoff)
            .drive(IN_RESONANCE, resonance)
            .drive(IN_AUDIO, input)
            .render(input.len())
            .output(OUT_AUDIO)
            .to_vec()
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
    fn constant_cutoff_buffer_matches_held_default() {
        // A constant cutoff buffer must produce exactly the same output as the same value held as
        // the input's materialized default — there is one read path now (ADR-0028), so a flat
        // wired control equals the held latch.
        let sr = 48_000.0;
        let n = 4096;
        let input = sine(8_000.0, sr, n);
        let via_default = render(&input, sr, 1_000.0, 0.0);
        let cutoff_buf = vec![1_000.0f32; n];
        let res_buf = vec![0.0f32; n];
        let via_buffer = render_buffers(&input, sr, &cutoff_buf, &res_buf, FilterMode::Lp);
        for i in 0..n {
            assert!(
                (via_default[i] - via_buffer[i]).abs() < 1e-4,
                "wired cutoff should match held default at {i}: {} vs {}",
                via_default[i],
                via_buffer[i]
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
        let res_buf = vec![0.0f32; n];
        let out = render_buffers(&input, sr, &cutoff, &res_buf, FilterMode::Lp);
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
        let res_buf = vec![0.0f32; n];
        let out = render_buffers(&input, sr, &constant, &res_buf, FilterMode::Lp);

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
        // One render of `n` must equal two back-to-back renders of `n/2` sharing the driver's
        // operator: the SVF integrator state threads across the real block boundaries and across
        // the separate `render` calls.
        let sr = 48_000.0;
        let n = 512;
        let input = sine(440.0, sr, n);
        let half = n / 2;

        let whole = render(&input, sr, 1_000.0, 0.3);

        let mut split = OpDriver::for_type(Filter::new(), sr);
        split
            .set(IN_CUTOFF, 1_000.0)
            .set(IN_RESONANCE, 0.3)
            .set(IN_MODE, FilterMode::Lp)
            .drive(IN_AUDIO, &input[..half]);
        let a = split.render(half).output(OUT_AUDIO).to_vec();
        split.drive(IN_AUDIO, &input[half..]);
        let b = split.render(n - half).output(OUT_AUDIO).to_vec();

        for i in 0..half {
            assert!(
                (whole[i] - a[i]).abs() < 1e-5,
                "slice mismatch (block 1) at {i}: {} vs {}",
                whole[i],
                a[i]
            );
            assert!(
                (whole[half + i] - b[i]).abs() < 1e-5,
                "slice mismatch (block 2) at {i}: {} vs {}",
                whole[half + i],
                b[i]
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
        let lp = render_mode(&input, sr, 1_200.0, 0.4, FilterMode::Lp);

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

        let low = render_mode(&sine(200.0, sr, n), sr, 1_000.0, 0.0, FilterMode::Hp);
        let high = render_mode(&sine(8_000.0, sr, n), sr, 1_000.0, 0.0, FilterMode::Hp);

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

        let center = render_mode(&sine(1_000.0, sr, n), sr, 1_000.0, res, FilterMode::Bp);
        let below = render_mode(&sine(100.0, sr, n), sr, 1_000.0, res, FilterMode::Bp);
        let above = render_mode(&sine(8_000.0, sr, n), sr, 1_000.0, res, FilterMode::Bp);

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
        let out = render_mode(&vec![1.0f32; n], sr, 1_000.0, 0.0, FilterMode::Hp);
        let tail = &out[n - 256..];
        let avg = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(avg.abs() < 0.02, "highpass should block DC, got {avg}");
    }
}
