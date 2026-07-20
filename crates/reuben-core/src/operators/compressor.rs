//! compressor — feedforward sidechain compressor for EDM-style kick pumping.
//!
//! A single-band feedforward dynamics processor after the canonical log-domain design (Giannoulis,
//! Massberg & Reiss, *Digital Dynamic Range Compressor Design — A Tutorial and Analysis*, JAES
//! 2012): detect the **key** signal's level, compute the gain reduction from `threshold`/`ratio`,
//! smooth it with attack/release ballistics, and apply it to the **main** signal. Wire a kick into
//! the `sidechain` key and it ducks the main in time with the beat — the classic pump.
//!
//! Per sample, on the key `k`:
//! 1. **Key high-pass** — `k` runs through a 2-pole high-pass (`key_hp`, the shared Cytomic SVF's
//!    `hp` tap) so low-frequency energy can be kept out of the detector. At the 20 Hz default it is
//!    ~transparent; raise it to stop a boomy key from over-triggering.
//! 2. **Peak detect + hard-knee gain computer** (log domain) — with level `x = 20·log10|k|` and
//!    threshold `T`, ratio `R`, the raw gain reduction is `c = (x − T)·(1 − 1/R)` when `x > T`,
//!    else `0`. Peak (not RMS) detection keeps the pump snappy on the kick transient. Below
//!    threshold the log is skipped (compared in the linear domain against `10^(T/20)`).
//! 3. **Branched one-pole ballistics** — smooth `c` into the running reduction `env`: attack
//!    coefficient while the reduction is growing (`c > env`), release while it recovers, each
//!    `α = exp(−1/(τ·fs))`. `release` is the musical knob — it shapes the ramp back up between kicks.
//! 4. **Apply** — `out = 10^((makeup − env)/20) · main`. The reduction only ever attenuates
//!    (`env ≥ 0`); `makeup` trims the output back up. Fully recovered (`env ≈ 0`) the exp is
//!    skipped, so the between-kick path costs nothing beyond the makeup trim.
//!
//! The **key source** follows the sidechain when it is wired and falls back to the main input when
//! it is not (ADR-0031 materialize: an unwired bare Signal reads `io.varying == false`, a wired
//! source `true`). So unwired it is a plain self-keyed compressor; wire a kick and it becomes a
//! sidechain pumper — no mode switch. Controls are held Values (block-sliced at changes); the
//! detector and ballistics coefficients are recomputed once per block, not per sample.
//!
//! - input 0: `audio` — the main signal to compress / duck.
//! - input 1: `sidechain` — the external key (a kick). Unwired ⇒ the detector keys off `audio`.
//! - input 2: `threshold` (dB) — level above which gain reduction begins.
//! - input 3: `ratio` (:1) — compression ratio; `1` is a bypass, higher ducks harder.
//! - input 4: `attack` (ms) — how fast the reduction engages on the key transient.
//! - input 5: `release` (ms) — how fast the main recovers; the shape of the pump.
//! - input 6: `makeup` (dB) — output gain applied after compression.
//! - input 7: `key_hp` (Hz) — high-pass on the key before detection (20 Hz ≈ off).
//! - output 0: `audio` — the compressed / ducked signal.

use crate::descriptor::Descriptor;
use crate::dsp::svf::{Svf, SvfCoeffs};
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Compressor {
    type_name: "compressor",
    inputs: { audio:     f32_buffer,
              sidechain: f32_buffer,
              threshold: f32 { -60.0..=0.0,    default -24.0, "dB", lin },
              ratio:     f32 { 1.0..=20.0,     default 4.0,   ":1", lin },
              attack:    f32 { 0.1..=100.0,    default 5.0,   "ms", exp },
              release:   f32 { 1.0..=2000.0,   default 150.0, "ms", exp },
              makeup:    f32 { -24.0..=24.0,   default 0.0,   "dB", lin },
              key_hp:    f32 { 20.0..=2000.0,  default 20.0,  "Hz", exp } },
    outputs: { audio: f32_buffer },
});

/// `ln(10)/20` — converts dB to a linear factor via `exp(dB · this)` (i.e. `10^(dB/20)`), so the
/// hot loop never calls `powf(10, …)`.
const LN10_OVER_20: f32 = 0.115_129_255;
/// Below this much reduction (dB) the gain is indistinguishable from the makeup trim, so the loop
/// skips the `exp` — the common recovered / between-kick sample costs nothing extra.
const GAIN_EPS_DB: f32 = 1.0e-4;

#[derive(Default)]
pub struct Compressor {
    /// Smoothed gain reduction in dB (`≥ 0`) — the ballistics state, continuous across blocks /
    /// slices, reset on `spawn`.
    env: f32,
    /// Sidechain-key high-pass state (shared Cytomic SVF), continuous across blocks. Copied to a
    /// local for the block loop and written back once (#169).
    svf: Svf,
}

impl Compressor {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Attack/release one-pole smoothing coefficient `α = exp(−1/(τ·fs))` for a time in **ms** (τ is
/// the time to reach ~63% of a step). A degenerate sample rate ⇒ `0` (instant follow).
#[inline]
fn ballistics_coeff(time_ms: f32, sample_rate: f32, valid_sr: bool) -> f32 {
    if !valid_sr {
        return 0.0;
    }
    let tau = (time_ms * 1.0e-3).max(1.0e-6);
    (-1.0 / (tau * sample_rate)).exp()
}

impl Operator for Compressor {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();
        let valid_sr = sample_rate > 0.0;

        // Held controls — constant for this (sub)block (the engine block-slices at each change).
        let threshold_db = io.read(IN_THRESHOLD);
        let ratio = io.read(IN_RATIO).max(1.0);
        let attack_ms = io.read(IN_ATTACK);
        let release_ms = io.read(IN_RELEASE);
        let makeup_db = io.read(IN_MAKEUP);
        let key_hp = io.read(IN_KEY_HP);

        // Per-block derived coefficients (done once, reused every sample — like the filter's SVF
        // coeffs): the gain-computer slope, the linear threshold for the below-threshold early-out,
        // the makeup factor, the attack/release poles, and the key high-pass coefficients.
        let slope = 1.0 - 1.0 / ratio; // (1 − 1/R) ≥ 0; ratio 1 ⇒ 0 ⇒ bypass.
        let thresh_lin = (threshold_db * LN10_OVER_20).exp(); // 10^(T/20)
        let makeup_lin = (makeup_db * LN10_OVER_20).exp();
        let alpha_a = ballistics_coeff(attack_ms, sample_rate, valid_sr);
        let alpha_r = ballistics_coeff(release_ms, sample_rate, valid_sr);
        let coeffs = if valid_sr {
            SvfCoeffs::new(key_hp, 0.0, sample_rate)
        } else {
            SvfCoeffs::default()
        };

        // Key = the sidechain when wired, else the main input (the unwired-fallback contract). One
        // per-block decision: a wired Signal reads `varying == true`, an unwired bare Signal (which
        // materializes constant silence) reads `false` from block 0 (ADR-0031/0037).
        let key_wired = io.varying(IN_SIDECHAIN);

        // Resolve slices once (each is exactly `n` samples — the buffer-presence invariant). The
        // reads borrow the arena (`'a`), so taking the output's mutable borrow after is fine.
        let audio = io.read(IN_AUDIO);
        let sidechain = io.read(IN_SIDECHAIN);
        let key_src = if key_wired { sidechain } else { audio };
        let out = io.write(OUT_AUDIO);

        let mut env = self.env; // smoothed reduction (dB), threaded across blocks
        let mut svf = self.svf; // key high-pass state, threaded across blocks
        for i in 0..n {
            // Detect the high-passed key's rectified peak.
            let k = if valid_sr {
                svf.tick(key_src[i], coeffs).hp
            } else {
                key_src[i]
            };
            let k_abs = k.abs();
            // Hard-knee gain computer in the log domain. Below threshold the reduction is exactly
            // zero, so the compare stays linear and the log is only paid when actually compressing.
            let target = if k_abs > thresh_lin {
                (k_abs.log10() * 20.0 - threshold_db) * slope
            } else {
                0.0
            };
            // Branched one-pole: attack when the reduction is growing, release when recovering.
            let coeff = if target > env { alpha_a } else { alpha_r };
            env += (1.0 - coeff) * (target - env);
            // makeup − reduction, as a linear gain on the main signal.
            let gain = if env > GAIN_EPS_DB {
                makeup_lin * (-env * LN10_OVER_20).exp()
            } else {
                makeup_lin
            };
            out[i] = gain * audio[i];
        }
        self.env = env;
        self.svf = svf;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Compressor);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// A constant-magnitude AC signal: a period-4 square (12 kHz at 48 kHz, well inside the key
    /// high-pass passband) whose rectified value is exactly `amp` every sample. That makes the
    /// detected level — and so the steady-state gain reduction — a clean function of `amp`, with no
    /// peak/RMS ambiguity or per-cycle ripple to average out.
    fn ac(n: usize, amp: f32) -> Vec<f32> {
        (0..n)
            .map(|i| if (i / 2) % 2 == 0 { amp } else { -amp })
            .collect()
    }

    fn mean_abs(buf: &[f32]) -> f32 {
        buf.iter().map(|x| x.abs()).sum::<f32>() / buf.len() as f32
    }

    #[test]
    fn silence_in_stays_silent() {
        // Unwired audio materializes silence; gain · 0 = 0 at any settings.
        let out = OpDriver::for_type(Compressor::new(), SR)
            .render(4_096)
            .output(OUT_AUDIO)
            .to_vec();
        for (i, &s) in out.iter().enumerate() {
            assert!(s.abs() < 1e-9, "sample {i} should be silent, got {s}");
        }
    }

    #[test]
    fn below_threshold_passes_through_at_unity() {
        // -30.5 dBFS main (amp 0.03), self-keyed, below the -24 dB threshold ⇒ no reduction, and
        // makeup 0 ⇒ unity gain ⇒ output is the input untouched.
        let n = 24_000;
        let input = ac(n, 0.03);
        let out = OpDriver::for_type(Compressor::new(), SR)
            .drive(IN_AUDIO, &input)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        for i in 12_000..n {
            assert!(
                (out[i] - input[i]).abs() < 1e-3,
                "below threshold must pass clean at {i}: {} vs {}",
                out[i],
                input[i]
            );
        }
    }

    /// Steady-state gain reduction (dB) of a self-keyed constant-magnitude signal at `amp`/`ratio`.
    fn reduction_db(amp: f32, ratio: f32) -> f32 {
        let n = 48_000;
        let input = ac(n, amp);
        let out = OpDriver::for_type(Compressor::new(), SR)
            .drive(IN_AUDIO, &input)
            .set(IN_RATIO, ratio)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        // Skip past the attack (default 5 ms) — the tail is fully settled at env == target.
        let g = mean_abs(&out[24_000..]) / amp;
        -20.0 * g.log10()
    }

    #[test]
    fn above_threshold_compresses_by_the_ratio_law() {
        // The core oracle: amp 0.5 = -6.02 dBFS, threshold -24, ratio 4 ⇒ ideal reduction
        // (−6.02 − (−24))·(1 − 1/4) = 13.48 dB. Tolerance absorbs the ~0 dB key-HP loss + detector
        // ripple.
        let r4 = reduction_db(0.5, 4.0);
        assert!(
            (r4 - 13.48).abs() < 2.5,
            "ratio-4 reduction {r4} dB should be near the ideal 13.48 dB"
        );
        // Monotone in ratio: 8:1 ducks harder than 4:1 ducks harder than 2:1.
        let r2 = reduction_db(0.5, 2.0);
        let r8 = reduction_db(0.5, 8.0);
        assert!(
            r8 > r4 && r4 > r2,
            "reduction must grow with ratio: {r2} < {r4} < {r8}"
        );
    }

    #[test]
    fn sidechain_key_ducks_the_main_signal() {
        // The headline: a sub-threshold main (amp 0.05 = -26 dBFS, below -24 so it never
        // self-compresses) plus a loud kick burst on the sidechain. The main ducks under the kick
        // and recovers after it.
        let n = 48_000;
        let audio = ac(n, 0.05);
        let mut kick = vec![0.0; n];
        kick[..6_000].copy_from_slice(&ac(6_000, 1.0)); // 0 dBFS kick for the first 125 ms
        let ducked = OpDriver::for_type(Compressor::new(), SR)
            .drive(IN_AUDIO, &audio)
            .drive(IN_SIDECHAIN, &kick)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        // Under the kick (past the attack) the main is pushed well below its own level.
        let during = mean_abs(&ducked[3_000..6_000]);
        assert!(
            during < 0.05 * 0.5,
            "the kick must duck the main: {during} vs input 0.05"
        );
        // Long after the kick (release 150 ms ≈ 7.2 k samples) the main is back.
        let after = mean_abs(&ducked[40_000..]);
        assert!(
            (after - 0.05).abs() < 0.05 * 0.15,
            "the main must recover after the kick: {after} vs 0.05"
        );
        // Control: with no sidechain wired, the sub-threshold main passes clean — proof the kick
        // drove the ducking, not the main self-keying.
        let clean = OpDriver::for_type(Compressor::new(), SR)
            .drive(IN_AUDIO, &audio)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        assert!(
            (mean_abs(&clean[3_000..6_000]) - 0.05).abs() < 1e-3,
            "an unkeyed sub-threshold main must pass untouched"
        );
    }

    #[test]
    fn unwired_sidechain_self_keys_wired_silence_does_not() {
        // The fallback contract, both directions. Loud main (amp 0.5, above threshold).
        let n = 24_000;
        let loud = ac(n, 0.5);
        // Sidechain unwired ⇒ detector falls back to the main ⇒ it self-compresses.
        let self_keyed = OpDriver::for_type(Compressor::new(), SR)
            .drive(IN_AUDIO, &loud)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        let self_gain = mean_abs(&self_keyed[12_000..]) / 0.5;
        assert!(
            self_gain < 0.7,
            "an unwired sidechain must self-key and compress: gain {self_gain}"
        );
        // Sidechain wired to silence ⇒ detector keys off the (silent) sidechain ⇒ no compression.
        let keyed_silent = OpDriver::for_type(Compressor::new(), SR)
            .drive(IN_AUDIO, &loud)
            .drive(IN_SIDECHAIN, &vec![0.0; n])
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        for i in 12_000..n {
            assert!(
                (keyed_silent[i] - loud[i]).abs() < 1e-3,
                "a silent wired key must pass the main clean at {i}"
            );
        }
    }

    #[test]
    fn makeup_is_a_pure_post_gain() {
        // Below-threshold main (unity compression) with +6 dB makeup ⇒ output = input · 10^(6/20).
        let n = 24_000;
        let input = ac(n, 0.03);
        let out = OpDriver::for_type(Compressor::new(), SR)
            .drive(IN_AUDIO, &input)
            .set(IN_MAKEUP, 6.0)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        let expected = 0.03 * 10f32.powf(6.0 / 20.0);
        let got = mean_abs(&out[12_000..]);
        assert!(
            (got - expected).abs() < 1e-3,
            "makeup should post-scale the output: {got} vs {expected}"
        );
    }

    #[test]
    fn longer_release_recovers_more_slowly() {
        // The pump-shape oracle: a kick then silence on the key; a longer release keeps the main
        // ducked longer, which is exactly the tempo-locked pump.
        let n = 24_000;
        let audio = ac(n, 0.05);
        let mut kick = vec![0.0; n];
        kick[..3_000].copy_from_slice(&ac(3_000, 1.0));
        let run = |release_ms: f32| {
            OpDriver::for_type(Compressor::new(), SR)
                .drive(IN_AUDIO, &audio)
                .drive(IN_SIDECHAIN, &kick)
                .set(IN_RELEASE, release_ms)
                .render(n)
                .output(OUT_AUDIO)
                .to_vec()
        };
        let fast = run(50.0);
        let slow = run(500.0);
        // Probe 50 ms after the kick ends: the slow release is still markedly more ducked.
        let probe = 3_000 + 2_400;
        let f = mean_abs(&fast[probe..probe + 240]);
        let s = mean_abs(&slow[probe..probe + 240]);
        assert!(
            s < f * 0.9,
            "longer release must keep the main ducked longer: slow {s} vs fast {f}"
        );
    }

    #[test]
    fn faster_attack_ducks_sooner() {
        // A continuous loud key from frame 0; shortly after onset the fast attack is already
        // ducked while the slow one still lets the main through.
        let n = 12_000;
        let audio = ac(n, 0.05);
        let kick = ac(n, 1.0);
        let run = |attack_ms: f32| {
            OpDriver::for_type(Compressor::new(), SR)
                .drive(IN_AUDIO, &audio)
                .drive(IN_SIDECHAIN, &kick)
                .set(IN_ATTACK, attack_ms)
                .set(IN_RELEASE, 200.0)
                .render(n)
                .output(OUT_AUDIO)
                .to_vec()
        };
        let fast = run(0.5);
        let slow = run(50.0);
        let probe = 120; // 2.5 ms after onset
        let f = mean_abs(&fast[probe..probe + 48]);
        let s = mean_abs(&slow[probe..probe + 48]);
        assert!(
            f < s * 0.9,
            "faster attack must duck sooner: fast {f} vs slow {s}"
        );
    }

    #[test]
    fn key_highpass_removes_low_key_content() {
        // A low (60 Hz) key at full level. Wide-open HP (20 Hz) lets it duck the main; an HP up at
        // 1 kHz strips the key below threshold, so the main is barely touched — the sidechain
        // filter working.
        let n = 24_000;
        let audio = ac(n, 0.05);
        let low_key: Vec<f32> = (0..n)
            .map(|i| (std::f32::consts::TAU * 60.0 * i as f32 / SR).sin())
            .collect();
        let run = |key_hp: f32| {
            OpDriver::for_type(Compressor::new(), SR)
                .drive(IN_AUDIO, &audio)
                .drive(IN_SIDECHAIN, &low_key)
                .set(IN_KEY_HP, key_hp)
                .render(n)
                .output(OUT_AUDIO)
                .to_vec()
        };
        let open = mean_abs(&run(20.0)[12_000..]);
        let closed = mean_abs(&run(1_000.0)[12_000..]);
        assert!(
            open < closed * 0.8,
            "raising the key HP must reduce ducking of a low key: open {open} vs closed {closed}"
        );
        assert!(
            closed > 0.05 * 0.7,
            "a filtered-out key should barely duck the main: {closed}"
        );
    }

    #[test]
    fn state_is_continuous_across_render_calls() {
        // One render of 2n equals two back-to-back renders of n on the same driver: env AND the key
        // high-pass state both thread across the block boundary.
        let n = 1_000;
        let audio = ac(2 * n, 0.5);
        let mut kick = vec![0.0; 2 * n];
        kick[..1_500].copy_from_slice(&ac(1_500, 1.0));
        let whole = OpDriver::for_type(Compressor::new(), SR)
            .drive(IN_AUDIO, &audio)
            .drive(IN_SIDECHAIN, &kick)
            .render(2 * n)
            .output(OUT_AUDIO)
            .to_vec();

        let mut split = OpDriver::for_type(Compressor::new(), SR);
        split
            .drive(IN_AUDIO, &audio[..n])
            .drive(IN_SIDECHAIN, &kick[..n]);
        let a = split.render(n).output(OUT_AUDIO).to_vec();
        split
            .drive(IN_AUDIO, &audio[n..])
            .drive(IN_SIDECHAIN, &kick[n..]);
        let b = split.render(n).output(OUT_AUDIO).to_vec();

        for i in 0..n {
            assert!((a[i] - whole[i]).abs() < 1e-5, "block 1 differs at {i}");
            assert!(
                (b[i] - whole[n + i]).abs() < 1e-5,
                "block 2 differs at {i}: env / key-HP state must thread across renders"
            );
        }
    }

    #[test]
    fn spawned_compressor_starts_fresh() {
        // Charge the parent's ballistics + key-HP state with a loud self-keyed signal, then spawn:
        // the child on a sub-threshold input must pass at unity from the first sample, not replay
        // the parent's gain reduction.
        let mut parent = OpDriver::for_type(Compressor::new(), SR);
        parent.drive(IN_AUDIO, &ac(6_000, 1.0));
        parent.render(6_000);

        let mut child = parent.spawn();
        let quiet = ac(6_000, 0.03);
        let out = child
            .drive(IN_AUDIO, &quiet)
            .render(6_000)
            .output(OUT_AUDIO)
            .to_vec();
        for i in 0..240 {
            assert!(
                (out[i] - quiet[i]).abs() < 1e-3,
                "spawn must reset the reduction at {i}: {} vs {}",
                out[i],
                quiet[i]
            );
        }
    }
}
