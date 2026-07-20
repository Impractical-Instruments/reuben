//! saturator — warm tanh drive: peak-normalized saturation plus a post-shaper even-harmonic
//! `warmth` term, DC-blocked.
//!
//! Per sample: `s = tanh(drive·x) / tanh(drive)` — the division peak-normalizes, so a full-scale
//! input peaks near ±1 at any drive and cranking `drive` raises density (RMS), not peak level.
//! `warmth` then adds an even-order term on the *shaped* signal, `v = s + 0.5·warmth·s²`: second
//! harmonic strongest at gentle drive and receding as the wave squares off, the way driven
//! analog gear behaves. (Applied post-shaper because tanh crushes a pre-shaper x² term to
//! inaudibility past moderate drive; and since `s` is tanh-bounded, `v` is monotonic in `s` for
//! any input level — no fold-back on hot inputs.) The s² term leaves a signal-dependent DC
//! offset, so a one-pole ~5 Hz DC blocker sits after the shaper; its transient overshoot means
//! output peaks can briefly exceed ±level by ~15% worst-case.
//!
//! `drive`/`warmth` are Signal inputs with scalar defaults: knob-set they materialize
//! as held buffers, or an LFO/envelope wires straight in for audio-rate modulation. `level` is a
//! held output trim after the shaper.
//!
//! - input 0: `audio` — the signal to saturate.
//! - input 1: `drive` (x, exp) — pre-gain into the tanh shaper; clamped to its declared range so
//!   a wild wired signal can't blow up the normalization.
//! - input 2: `warmth` — 0..1 even-harmonic amount.
//! - input 3: `level` (x) — output trim.
//! - output 0: `audio` — the saturated signal.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract: one declaration -> IN_/OUT_/C_ consts + Descriptor, no drift.
crate::operator_contract!(Saturator {
    inputs: { audio: f32_buffer,
              drive: f32_buffer { 1.0..=30.0, default 2.5, "x", exp },
              warmth: f32_buffer { 0.0..=1.0, default 0.4, "", lin },
              level: f32 { 0.0..=2.0, default 1.0, "x", lin } },
    outputs: { audio: f32_buffer },
});

// Named mirrors of the contract ranges above, for `process`'s runtime clamps. The
// `operator_contract!` grammar takes only numeric literals (and the handles it plants carry no
// min/max), so the declaration can't reference these consts — keep the two in lockstep.
/// `drive`'s declared range floor (mirrors the contract).
const DRIVE_MIN: f32 = 1.0;
/// `drive`'s declared range ceiling (mirrors the contract).
const DRIVE_MAX: f32 = 30.0;
/// `warmth`'s declared range floor (mirrors the contract).
const WARMTH_MIN: f32 = 0.0;
/// `warmth`'s declared range ceiling (mirrors the contract).
const WARMTH_MAX: f32 = 1.0;
/// `level`'s declared range floor (mirrors the contract).
const LEVEL_MIN: f32 = 0.0;
/// `level`'s declared range ceiling (mirrors the contract).
const LEVEL_MAX: f32 = 2.0;

/// Corner frequency of the post-shaper DC blocker — low enough to leave bass untouched, high
/// enough to drain the warmth term's offset within tens of milliseconds.
const DC_CORNER_HZ: f32 = 5.0;

#[derive(Default)]
pub struct Saturator {
    /// DC-blocker state (`y[n] = v[n] - v[n-1] + r·y[n-1]`), continuous across blocks / slices.
    /// Per-voice charge — reset on `spawn`.
    dc_x1: f32,
    dc_y1: f32,
}

impl Saturator {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Saturator {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();
        // One-pole DC-blocker pole for the ~DC_CORNER_HZ corner; a degenerate sample rate gets
        // r = 0 (the blocker becomes a plain differencer instead of dividing by zero).
        let r = if sample_rate > 0.0 {
            (1.0 - std::f32::consts::TAU * DC_CORNER_HZ / sample_rate).max(0.0)
        } else {
            0.0
        };
        let level = io.read(IN_LEVEL).clamp(LEVEL_MIN, LEVEL_MAX);

        // Resolve every slice once, outside the loop (the handle re-derivation cost — same hoist
        // as the filter). All are exactly `n` frames (buffer-presence invariant).
        let audio = io.read(IN_AUDIO);
        let drive = io.read(IN_DRIVE);
        let warmth = io.read(IN_WARMTH);
        let out = io.write(OUT_AUDIO);

        // The normalization `1/tanh(d)` is change-cached like the filter's coefficients: a held
        // drive costs one `tanh` per block, an audio-rate sweep recomputes per sample. The NaN
        // seed forces the first compute (NaN != anything). Drive is clamped to its declared
        // range so a wild wired signal can't zero the normalization denominator.
        let mut last_drive = f32::NAN;
        let (mut d, mut inv) = (1.0f32, 1.0f32);
        let (mut x1, mut y1) = (self.dc_x1, self.dc_y1);
        for i in 0..n {
            if drive[i] != last_drive {
                last_drive = drive[i];
                d = drive[i].clamp(DRIVE_MIN, DRIVE_MAX);
                inv = 1.0 / d.tanh();
            }
            let w = warmth[i].clamp(WARMTH_MIN, WARMTH_MAX);
            // Peak-normalized tanh drive: full-scale peaks stay near ±1 at any drive.
            let s = (d * audio[i]).tanh() * inv;
            // Even-harmonic warmth on the *shaped* signal (see module docs): monotonic for any
            // input because `s` is tanh-bounded, strongest at gentle drive.
            let v = s + 0.5 * w * s * s;
            // DC blocker drains the offset the s² term introduces.
            let y = v - x1 + r * y1;
            x1 = v;
            y1 = y;
            out[i] = level * y;
        }
        self.dc_x1 = x1;
        self.dc_y1 = y1;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Saturator);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;
    /// Test fundamental: 100 Hz is exactly 480 samples at 48 kHz, so analysis windows hold a
    /// whole number of cycles.
    const F0: f32 = 100.0;
    /// Samples to skip before analysis — the ~5 Hz DC blocker (τ ≈ 32 ms) has fully settled
    /// after 0.25 s.
    const SETTLE: usize = 12_000;
    /// Analysis window: 60 whole cycles of `F0`, so single-bin projections and means are exact.
    const WINDOW: usize = 28_800;

    fn sine(n: usize, freq: f32, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (std::f32::consts::TAU * freq * i as f32 / SR).sin())
            .collect()
    }

    /// Render `input` through a saturator at the given controls (full-length render).
    fn run(input: &[f32], drive: f32, warmth: f32, level: f32) -> Vec<f32> {
        OpDriver::for_type(Saturator::new(), SR)
            .drive(IN_AUDIO, input)
            .set(IN_DRIVE, drive)
            .set(IN_WARMTH, warmth)
            .set(IN_LEVEL, level)
            .render(input.len())
            .output(OUT_AUDIO)
            .to_vec()
    }

    /// Magnitude of the `freq` component of `buf` — a single-bin DFT projection, exact when
    /// `buf` spans whole cycles.
    fn harmonic_mag(buf: &[f32], freq: f32) -> f32 {
        let (mut s, mut c) = (0.0f64, 0.0f64);
        for (i, &x) in buf.iter().enumerate() {
            let ph = std::f64::consts::TAU * freq as f64 * i as f64 / SR as f64;
            s += x as f64 * ph.sin();
            c += x as f64 * ph.cos();
        }
        (2.0 * (s * s + c * c).sqrt() / buf.len() as f64) as f32
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / buf.len() as f64).sqrt() as f32
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, &x| m.max(x.abs()))
    }

    #[test]
    fn silence_in_stays_silent() {
        // Unwired audio materializes as silence; the shaper maps 0 -> 0 even at full warmth,
        // and the DC blocker holds no charge.
        let out = OpDriver::for_type(Saturator::new(), SR)
            .set(IN_WARMTH, 1.0)
            .render(4_096)
            .output(OUT_AUDIO)
            .to_vec();
        for (i, &s) in out.iter().enumerate() {
            assert!(s.abs() < 1e-6, "sample {i} should be silent, got {s}");
        }
    }

    #[test]
    fn declared_defaults_drive_a_sine_hot_and_bounded() {
        // Nothing set: drive/warmth/level materialize from their declared defaults
        // (2.5 / 0.4 / 1.0). A full-scale sine must come back loud but peak-normalized.
        let n = 48_000;
        let input = sine(n, F0, 1.0);
        let out = OpDriver::for_type(Saturator::new(), SR)
            .drive(IN_AUDIO, &input)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        assert!(out.iter().all(|s| s.is_finite()), "output must be finite");
        let tail = &out[SETTLE..];
        // Simulated: peak 1.06, RMS 0.86.
        assert!(
            peak(tail) <= 1.15,
            "peak-normalized, got peak {}",
            peak(tail)
        );
        assert!(
            rms(tail) >= 0.6,
            "defaults should saturate audibly, got rms {}",
            rms(tail)
        );
    }

    #[test]
    fn drive_flattens_the_waveform_toward_square() {
        // Peak normalization holds full-scale peaks near ±1 at every drive, so a squarer wave
        // shows up as RMS rising toward the peak (simulated: 0.75 -> 0.99), while the peak
        // itself stays bounded (the DC blocker's transient overshoot on a square is ~8%).
        let n = 48_000;
        let input = sine(n, F0, 1.0);
        let gentle = run(&input, 1.0, 0.0, 1.0);
        let slammed = run(&input, 30.0, 0.0, 1.0);
        let (r_gentle, r_slammed) = (rms(&gentle[SETTLE..]), rms(&slammed[SETTLE..]));
        assert!(
            peak(&slammed[SETTLE..]) <= 1.12,
            "slammed peaks stay normalized, got {}",
            peak(&slammed[SETTLE..])
        );
        assert!(
            r_slammed > r_gentle * 1.15,
            "more drive must flatten the waveform: rms {r_gentle} -> {r_slammed}"
        );
    }

    #[test]
    fn warmth_adds_second_harmonic() {
        // The oracle for "warm": warmth injects even-harmonic content. At default drive the
        // simulated H2/H1 is 0.167 at full warmth and ~1e-7 at zero (tanh alone is odd-symmetric
        // and the DC blocker is linear, so neither creates even harmonics).
        let n = 48_000;
        let input = sine(n, F0, 1.0);
        let clean = run(&input, 2.5, 0.0, 1.0);
        let warm = run(&input, 2.5, 1.0, 1.0);
        let win = |out: &[f32]| out[SETTLE..SETTLE + WINDOW].to_vec();
        let (h1_clean, h2_clean) = (
            harmonic_mag(&win(&clean), F0),
            harmonic_mag(&win(&clean), 2.0 * F0),
        );
        let (h1_warm, h2_warm) = (
            harmonic_mag(&win(&warm), F0),
            harmonic_mag(&win(&warm), 2.0 * F0),
        );
        assert!(
            h2_clean / h1_clean < 0.01,
            "zero warmth must stay odd-symmetric: H2/H1 = {}",
            h2_clean / h1_clean
        );
        assert!(
            h2_warm / h1_warm > 0.10,
            "full warmth must add strong second harmonic: H2/H1 = {}",
            h2_warm / h1_warm
        );
    }

    #[test]
    fn warm_output_stays_dc_free() {
        // The s² warmth term leaves a signal-dependent DC offset; the DC blocker must remove
        // it (simulated settled mean: ~8e-6).
        let n = 48_000;
        let out = run(&sine(n, F0, 1.0), 10.0, 1.0, 1.0);
        let tail = &out[SETTLE..SETTLE + WINDOW];
        let mean = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!(
            mean.abs() < 5e-3,
            "asymmetric shaping must not leave DC, got mean {mean}"
        );
    }

    #[test]
    fn level_is_a_pure_output_trim() {
        // Everything upstream of `level` is level-independent, so halving it must halve every
        // sample exactly (within f32 rounding).
        let n = 9_600;
        let input = sine(n, F0, 1.0);
        let unity = run(&input, 6.0, 0.5, 1.0);
        let halved = run(&input, 6.0, 0.5, 0.5);
        for i in 0..n {
            assert!(
                (halved[i] - 0.5 * unity[i]).abs() < 1e-5,
                "level must scale linearly at sample {i}: {} vs {}",
                halved[i],
                unity[i]
            );
        }
    }

    #[test]
    fn state_is_continuous_across_render_calls() {
        // One render of 2n must equal two back-to-back renders of n on the same driver: the
        // DC-blocker state threads across blocks and across separate `render` calls.
        let n = 1_000;
        let input = sine(2 * n, F0, 1.0);
        let whole = run(&input, 8.0, 1.0, 1.0);

        let mut split = OpDriver::for_type(Saturator::new(), SR);
        split
            .set(IN_DRIVE, 8.0)
            .set(IN_WARMTH, 1.0)
            .set(IN_LEVEL, 1.0);
        split.drive(IN_AUDIO, &input[..n]);
        let a = split.render(n).output(OUT_AUDIO).to_vec();
        split.drive(IN_AUDIO, &input[n..]);
        let b = split.render(n).output(OUT_AUDIO).to_vec();

        for i in 0..n {
            assert!((a[i] - whole[i]).abs() < 1e-5, "first half differs at {i}");
            assert!(
                (b[i] - whole[n + i]).abs() < 1e-5,
                "second half differs at {i}: DC-blocker state must thread across renders"
            );
        }
    }

    #[test]
    fn spawned_saturator_starts_fresh() {
        // Charge the parent's DC-blocker state with a hot asymmetric signal, then spawn: the
        // voice copy must start silent on silence, not replay the parent's charge.
        let mut parent = OpDriver::for_type(Saturator::new(), SR);
        parent.set(IN_DRIVE, 30.0).set(IN_WARMTH, 1.0);
        let input = sine(4_096, F0, 1.0);
        parent.drive(IN_AUDIO, &input);
        parent.render(4_096);

        let mut child = parent.spawn();
        let out = child
            .set(IN_WARMTH, 1.0)
            .render(512)
            .output(OUT_AUDIO)
            .to_vec();
        for (i, &s) in out.iter().enumerate() {
            assert!(
                s.abs() < 1e-6,
                "spawned voice must start silent, got {s} at {i}"
            );
        }
    }
}
