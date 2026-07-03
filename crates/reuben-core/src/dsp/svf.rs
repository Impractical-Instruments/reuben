//! Cytomic / TPT state-variable filter core (Andrew Simper's trapezoid-integrated SVF).
//!
//! One integrator pair produces all three classic responses from the same state, so a
//! single [`Svf::tick`] yields the lowpass, bandpass, and highpass taps together
//! ([`SvfTaps`]); callers pick the tap(s) they need and LLVM dead-codes the rest.
//!
//! This is the one shared SVF — `filter` and `djfilter` both embed it (#169) — and it is
//! shaped for the render thread:
//!
//! - [`SvfCoeffs`] is precomputed from (cutoff, resonance, sample rate) outside the sample
//!   loop; [`Svf::tick`] is pure arithmetic, no `tan`.
//! - [`Svf`] is a tiny `Copy` value. A `process` loop copies it to a local, ticks that,
//!   and stores it back once per block — keeping the integrators in registers instead of
//!   spilling them to the operator's fields every sample (#169).

/// Precomputed TPT / zero-delay-feedback SVF coefficients for one (cutoff, resonance,
/// sample rate) triple. Compute via [`SvfCoeffs::new`] whenever a control changes; reuse
/// freely while controls hold (the mapping is pure, so caching is bit-identical).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SvfCoeffs {
    k: f32,
    a1: f32,
    a2: f32,
    a3: f32,
}

impl SvfCoeffs {
    /// Coefficients for a cutoff (Hz) and resonance (0..1). Cutoff is clamped to a safe
    /// range so `tan` never blows up; resonance maps to damping `k = 1/Q` (k = 2 ⇒ no
    /// resonance, smaller k ⇒ more), clamped away from 0 for stability.
    #[inline]
    pub fn new(cutoff: f32, resonance: f32, sample_rate: f32) -> Self {
        let cutoff = cutoff.clamp(20.0, 0.45 * sample_rate);
        let k = (2.0 - 1.9 * resonance.clamp(0.0, 1.0)).max(0.1);
        let g = (std::f32::consts::PI * cutoff / sample_rate).tan();
        let a1 = 1.0 / (1.0 + g * (g + k));
        let a2 = g * a1;
        let a3 = g * a2;
        Self { k, a1, a2, a3 }
    }
}

/// The three simultaneous responses of one SVF step. `lp` is bit-identical to the
/// pre-extraction `filter`/`djfilter` lowpass; `bp = v1`; `hp = x - k·bp - lp` (standard
/// Cytomic). Take what you need — an unused tap costs nothing after inlining.
#[derive(Clone, Copy, Debug)]
pub struct SvfTaps {
    pub lp: f32,
    pub bp: f32,
    pub hp: f32,
}

/// SVF integrator state (`ic1eq`/`ic2eq`), continuous across blocks. `Default` is the
/// quiescent filter (both integrators zero).
///
/// Deliberately a plain `Copy` value: in a `process` loop, do
/// `let mut svf = self.svf; … svf.tick(…) …; self.svf = svf;` so the state lives in
/// registers for the whole block and hits memory once (#169).
#[derive(Clone, Copy, Debug, Default)]
pub struct Svf {
    ic1eq: f32,
    ic2eq: f32,
}

impl Svf {
    /// Advance one sample against precomputed coefficients and return all three taps.
    #[inline]
    pub fn tick(&mut self, x: f32, c: SvfCoeffs) -> SvfTaps {
        let v3 = x - self.ic2eq;
        let v1 = c.a1 * self.ic1eq + c.a2 * v3;
        let v2 = self.ic2eq + c.a2 * self.ic1eq + c.a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        SvfTaps {
            lp: v2,
            bp: v1,
            hp: x - c.k * v1 - v2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    /// Run `input` through a fresh SVF at constant (cutoff, resonance), collecting all taps.
    fn run(input: &[f32], cutoff: f32, resonance: f32) -> Vec<SvfTaps> {
        let c = SvfCoeffs::new(cutoff, resonance, SR);
        let mut svf = Svf::default();
        input.iter().map(|&x| svf.tick(x, c)).collect()
    }

    fn sine(f: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * f * i as f32 / SR).sin())
            .collect()
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt()
    }

    /// Steady-state RMS of one tap for a pure tone (transient skipped).
    fn tap_rms(f: f32, cutoff: f32, resonance: f32, tap: fn(&SvfTaps) -> f32) -> f32 {
        let n = 8192;
        let warmup = 2048;
        let taps = run(&sine(f, n), cutoff, resonance);
        rms(&taps[warmup..].iter().map(tap).collect::<Vec<_>>())
    }

    #[test]
    fn lowpass_attenuates_high_more_than_low() {
        // Cutoff at 1 kHz: 200 Hz passes, 8 kHz is well above the cutoff.
        let low = tap_rms(200.0, 1_000.0, 0.0, |t| t.lp);
        let high = tap_rms(8_000.0, 1_000.0, 0.0, |t| t.lp);
        assert!(low > high * 4.0, "expected low ({low}) >> high ({high})");
    }

    #[test]
    fn highpass_attenuates_low_more_than_high() {
        // Mirror image: the hp tap passes 8 kHz and attenuates 200 Hz.
        let low = tap_rms(200.0, 1_000.0, 0.0, |t| t.hp);
        let high = tap_rms(8_000.0, 1_000.0, 0.0, |t| t.hp);
        assert!(high > low * 4.0, "expected high ({high}) >> low ({low})");
    }

    #[test]
    fn bandpass_peaks_near_cutoff() {
        // A bandpass centered at 1 kHz passes 1 kHz far more strongly than either a much
        // lower (100 Hz) or much higher (8 kHz) tone. A little resonance sharpens the peak.
        let res = 0.6;
        let center = tap_rms(1_000.0, 1_000.0, res, |t| t.bp);
        let below = tap_rms(100.0, 1_000.0, res, |t| t.bp);
        let above = tap_rms(8_000.0, 1_000.0, res, |t| t.bp);
        assert!(
            center > below * 2.0 && center > above * 2.0,
            "bandpass should peak at cutoff: center {center}, below {below}, above {above}"
        );
    }

    #[test]
    fn lowpass_passes_dc_near_unity_and_highpass_blocks_it() {
        // After settling on a constant input, lp sits at the input level and hp at ~0.
        let n = 8192;
        let taps = run(&vec![1.0f32; n], 1_000.0, 0.0);
        let tail = &taps[n - 256..];
        let lp_avg = tail.iter().map(|t| t.lp).sum::<f32>() / 256.0;
        let hp_avg = tail.iter().map(|t| t.hp).sum::<f32>() / 256.0;
        assert!(
            (lp_avg - 1.0).abs() < 0.01,
            "lowpass should pass DC at unity, got {lp_avg}"
        );
        assert!(
            hp_avg.abs() < 0.02,
            "highpass should block DC, got {hp_avg}"
        );
    }

    #[test]
    fn high_resonance_stays_bounded() {
        // Drive at the resonant frequency with maximum resonance: every tap stays finite
        // and sane (the k clamp in `SvfCoeffs::new` keeps the filter stable).
        let taps = run(&sine(1_000.0, 8192), 1_000.0, 1.0);
        for (i, t) in taps.iter().enumerate() {
            for (name, v) in [("lp", t.lp), ("bp", t.bp), ("hp", t.hp)] {
                assert!(v.is_finite(), "{name} sample {i} not finite: {v}");
                assert!(v.abs() < 1_000.0, "{name} sample {i} unbounded: {v}");
            }
        }
    }

    #[test]
    fn state_threads_across_value_copies() {
        // Copying the state out mid-stream and resuming from the copy is seamless: one
        // continuous run equals two half runs threaded through the copied value. This is
        // exactly the once-per-block writeback pattern operators use (#169).
        let n = 512;
        let input = sine(440.0, n);
        let c = SvfCoeffs::new(1_000.0, 0.3, SR);

        let mut whole = Svf::default();
        let whole_out: Vec<f32> = input.iter().map(|&x| whole.tick(x, c).lp).collect();

        let state = Svf::default();
        let mut block1 = state; // "block 1" register copy
        let a: Vec<f32> = input[..n / 2]
            .iter()
            .map(|&x| block1.tick(x, c).lp)
            .collect();
        let state = block1; // once-per-block writeback
        let mut block2 = state; // "block 2" register copy
        let b: Vec<f32> = input[n / 2..]
            .iter()
            .map(|&x| block2.tick(x, c).lp)
            .collect();

        for i in 0..n / 2 {
            assert_eq!(
                whole_out[i].to_bits(),
                a[i].to_bits(),
                "block 1 diverged at {i}"
            );
            assert_eq!(
                whole_out[n / 2 + i].to_bits(),
                b[i].to_bits(),
                "block 2 diverged at {i}"
            );
        }
    }

    #[test]
    fn coeffs_clamp_hostile_inputs() {
        // Out-of-range controls must still yield finite, stable coefficients: cutoff far
        // beyond Nyquist, negative cutoff, and resonance outside 0..1 all clamp.
        for (cutoff, resonance) in [
            (1.0e9, 0.5),
            (-500.0, 0.5),
            (1_000.0, -3.0),
            (1_000.0, 42.0),
        ] {
            let c = SvfCoeffs::new(cutoff, resonance, SR);
            let mut svf = Svf::default();
            for &x in &sine(1_000.0, 1024) {
                let t = svf.tick(x, c);
                assert!(
                    t.lp.is_finite() && t.bp.is_finite() && t.hp.is_finite(),
                    "unstable output for cutoff {cutoff}, resonance {resonance}"
                );
            }
        }
    }
}
