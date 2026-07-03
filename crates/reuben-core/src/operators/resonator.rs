//! Resonator — a modal resonator (Mutable Instruments *Rings*-inspired).
//!
//! A bank of `NUM_MODES` tuned two-pole resonators ("modes") excited in parallel. The excitation
//! is the sum of an **external input** (`in`, the audio-effect / "excite it with anything" path)
//! and an **internal mallet** — a short brightness-filtered noise burst fired by a rising edge on
//! `gate` (the ping / strum trigger). Output is **pure wet**: the ringing bank only, so dry/wet
//! blending (if wanted) is a downstream `mul`/`add` patch.
//!
//! It is authored single-Voice (ADR-0032): one mono stream, hosted by the Voicer via the standard
//! `freq`/`gate` voice interface. So the existing `strum` op → Voicer → per-voice `gate` plucks it
//! for free, and a note-on simply pings it at the voice's `freq`.
//!
//! Macro controls map the *Rings* panel (all held `f32` Values, ADR-0031 — read once per
//! block-slice; the bank's coefficients are recomputed only when one changes, à la `filter`, so a
//! 32-mode bank stays cheap on the hot path):
//! - `freq` — the resonant fundamental (Hz). Mode `i` sits at partial `i+1`.
//! - `structure` — harmonic → inharmonic: stretches the partials off the integer series
//!   (`f_i = freq·n·√(1 + B·n²)`, normalized so the fundamental stays exact). 0 = a harmonic
//!   string/tube; up = bell/bar-like.
//! - `brightness` — spectral tilt: weights the partial amplitudes (`brightness^i`) and the mallet
//!   noise colour. Dark = fundamental only; bright = a full spectrum.
//! - `damping` — ring/decay time: maps to the per-mode pole radius (longer = more sustain). Higher
//!   modes decay faster, as real resonators do.
//! - `position` — excitation node comb: attenuates partials with a node at the striking point
//!   (`|sin(π·n·pos)|`), the classic struck-string/plate timbral shaper.
//!
//! - input 0: `in` (`Buffer`) — external excitation; unwired it materializes to silence.
//! - input 1: `freq` (`Float`, Hz) — resonant fundamental (default 220).
//! - input 2: `gate` (`Float`) — rising edge fires the mallet; the edge value is the velocity.
//! - input 3: `structure` (`Float`) — partial inharmonicity 0..1.
//! - input 4: `brightness` (`Float`) — spectral tilt 0..1.
//! - input 5: `damping` (`Float`) — ring time 0..1.
//! - input 6: `position` (`Float`) — excitation comb 0..1.
//! - output 0: `out` (`Buffer`) — the pure-wet resonator output.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0030/0031): one declaration -> IN_/OUT_ consts + Descriptor.
// `in` is a per-sample Signal; `freq` and the four macros are held `f32` Values (block-rate, the
// bank recomputes its coefficients only when one changes); `out` is the wet Signal.
crate::operator_contract!(Resonator {
    inputs:  { in: f32_buffer,
               freq:       f32 { 20.0..=8000.0, default 220.0, "Hz", exp },
               gate:       f32 { 0.0..=1.0,     default 0.0,   "",   lin },
               structure:  f32 { 0.0..=1.0,     default 0.25,  "",   lin },
               brightness: f32 { 0.0..=1.0,     default 0.5,   "",   lin },
               damping:    f32 { 0.0..=1.0,     default 0.7,   "",   lin },
               position:   f32 { 0.0..=1.0,     default 0.3,   "",   lin } },
    outputs: { out: f32_buffer },
});

/// Number of modes (resonant partials) in the bank. Fixed so `process` allocates nothing; high
/// modes above Nyquist are simply muted (gain 0).
const NUM_MODES: usize = 32;
/// `structure` → inharmonicity coefficient `B`. Tuned so the default (0.25) is mildly stretched and
/// the maximum is clearly bell-like without being unstable.
const INHARM: f32 = 0.1;
/// How much faster each successive mode decays (`T_i = T0 / (1 + HF_DAMP·i)`) — real resonators
/// shed their high partials first.
const HF_DAMP: f32 = 0.5;
/// Fundamental decay-time range (seconds) mapped exponentially by `damping` (0 → short, 1 → long).
const T_MIN: f32 = 0.02;
const T_MAX: f32 = 8.0;
/// Mallet noise-burst length, seconds (~3 ms — a struck/blown attack, not a click).
const BURST_SECS: f32 = 0.003;
/// Boosts the short mallet burst so the ring is clearly audible.
const EXC_GAIN: f32 = 4.0;
/// Output trim keeping the summed bank at a comfortable level.
const MASTER_GAIN: f32 = 0.5;
/// Fixed deterministic PRNG seed a fresh / spawned Resonator starts from (xorshift can't leave 0).
const SEED: u32 = 0x2545_F491;

pub struct Resonator {
    /// Per-mode two-pole resonator coefficients (recomputed only when a control changes).
    /// `y[n] = g·x[n] + c·y[n-1] - d·y[n-2]`.
    c: [f32; NUM_MODES],
    d: [f32; NUM_MODES],
    g: [f32; NUM_MODES],
    /// Per-mode delay state (the ring), continuous across blocks; reset on `spawn`.
    y1: [f32; NUM_MODES],
    y2: [f32; NUM_MODES],

    /// Last control values the coefficients were computed for; NaN forces a first compute.
    last_freq: f32,
    last_structure: f32,
    last_brightness: f32,
    last_damping: f32,
    last_position: f32,
    last_sr: f32,

    /// xorshift32 PRNG state for the mallet noise (continuous across blocks; reset to SEED).
    rng: u32,
    /// One-pole lowpass state colouring the mallet noise.
    exc_lp: f32,
    /// Samples remaining in the current mallet burst (0 = idle). Threads across blocks.
    burst_remaining: i32,
    /// Length of the current burst in samples (for the linear burst envelope).
    burst_len: i32,
    /// Amplitude (velocity) of the current burst.
    burst_amp: f32,
    /// Whether `gate` was high on the previous block-slice (edge detection).
    prev_gate: bool,
}

impl Default for Resonator {
    fn default() -> Self {
        Self {
            c: [0.0; NUM_MODES],
            d: [0.0; NUM_MODES],
            g: [0.0; NUM_MODES],
            y1: [0.0; NUM_MODES],
            y2: [0.0; NUM_MODES],
            last_freq: f32::NAN,
            last_structure: f32::NAN,
            last_brightness: f32::NAN,
            last_damping: f32::NAN,
            last_position: f32::NAN,
            last_sr: f32::NAN,
            rng: SEED,
            exc_lp: 0.0,
            burst_remaining: 0,
            burst_len: 0,
            burst_amp: 0.0,
            prev_gate: false,
        }
    }
}

impl Resonator {
    pub fn new() -> Self {
        Self::default()
    }

    /// One xorshift32 step → a fresh u32 (Marsaglia 13/17/5; never collapses to 0).
    #[inline]
    fn next_u32(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// Next white-noise sample in [-1, 1) (top 24 bits → even distribution).
    #[inline]
    fn next_noise(&mut self) -> f32 {
        let bits = self.next_u32() >> 8;
        (bits as f32) * (1.0 / (1u32 << 24) as f32) * 2.0 - 1.0
    }

    /// Recompute the per-mode coefficients from the held controls. Pure given its args, so reusing
    /// the cache when nothing changed is identical to recomputing. Called at most once per block.
    fn recompute(
        &mut self,
        freq: f32,
        structure: f32,
        brightness: f32,
        damping: f32,
        position: f32,
        sample_rate: f32,
    ) {
        let nyq = 0.45 * sample_rate;
        let b = structure.clamp(0.0, 1.0) * INHARM;
        // Normalize so mode 0 lands exactly on `freq` regardless of the stretch.
        let ratio1 = (1.0 + b).sqrt();
        let damping = damping.clamp(0.0, 1.0);
        let t0 = T_MIN * (T_MAX / T_MIN).powf(damping);
        // Map position to a musical strike point in [0.05, 0.5] — never the degenerate 0 (silence).
        let pos = 0.05 + 0.45 * position.clamp(0.0, 1.0);
        let brightness = brightness.clamp(0.0, 1.0);

        for i in 0..NUM_MODES {
            let n = (i + 1) as f32;
            let f = freq * n * (1.0 + b * n * n).sqrt() / ratio1;
            if f <= 0.0 || f >= nyq {
                self.c[i] = 0.0;
                self.d[i] = 0.0;
                self.g[i] = 0.0;
                continue;
            }
            let w = std::f32::consts::TAU * f / sample_rate;
            let t = t0 / (1.0 + HF_DAMP * i as f32);
            let r = (-1.0 / (t * sample_rate)).exp().min(0.99995);
            let comb = (std::f32::consts::PI * n * pos).sin().abs();
            let amp = brightness.powi(i as i32) * comb;
            self.c[i] = 2.0 * r * w.cos();
            self.d[i] = r * r;
            // (1 - r²) normalizes the resonant peak so the bank stays bounded as r → 1.
            self.g[i] = (1.0 - r * r) * amp;
        }
    }

    /// One sample of the internal mallet exciter (0 when idle). A brightness-filtered noise burst
    /// with a linear decay envelope, scaled by the trigger velocity.
    #[inline]
    fn exciter_step(&mut self, lp_alpha: f32) -> f32 {
        if self.burst_remaining <= 0 {
            return 0.0;
        }
        let env = self.burst_remaining as f32 / self.burst_len.max(1) as f32;
        let white = self.next_noise();
        self.exc_lp += lp_alpha * (white - self.exc_lp);
        self.burst_remaining -= 1;
        self.exc_lp * env * self.burst_amp * EXC_GAIN
    }
}

impl Operator for Resonator {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Held controls (ADR-0031): one read each, constant for this block-slice.
        let freq = io.read(IN_FREQ);
        let structure = io.read(IN_STRUCTURE);
        let brightness = io.read(IN_BRIGHTNESS);
        let damping = io.read(IN_DAMPING);
        let position = io.read(IN_POSITION);

        // Recompute the bank only when a control actually changed (NaN seed forces the first).
        if freq != self.last_freq
            || structure != self.last_structure
            || brightness != self.last_brightness
            || damping != self.last_damping
            || position != self.last_position
            || sample_rate != self.last_sr
        {
            self.recompute(freq, structure, brightness, damping, position, sample_rate);
            self.last_freq = freq;
            self.last_structure = structure;
            self.last_brightness = brightness;
            self.last_damping = damping;
            self.last_position = position;
            self.last_sr = sample_rate;
        }

        // `gate` is a held Value: the engine block-slices at each change, so the slice's frame 0 is
        // the edge — fire the mallet there (sample-accurate to the slice). `prev_gate` carries the
        // level across slices/blocks so a held gate fires exactly once.
        let gate = io.read(IN_GATE);
        let gate_hi = gate > 0.5;
        if gate_hi && !self.prev_gate {
            self.burst_len = ((BURST_SECS * sample_rate) as i32).max(1);
            self.burst_remaining = self.burst_len;
            self.burst_amp = gate.clamp(0.0, 1.0);
        }
        self.prev_gate = gate_hi;

        // Mallet noise colour: bright → a more open lowpass on the burst.
        let lp_alpha = 0.1 + 0.9 * brightness.clamp(0.0, 1.0);

        // `in` is a Signal — always a buffer (wired source or materialized silence). Resolve it and
        // the output buffer once (see filter.rs): the input read returns a block-lifetime slice, so
        // it coexists with the output's mutable borrow, and indexing flat locals avoids re-deriving
        // each slice from `io` per sample.
        let audio = io.read(IN_IN);
        let out = io.write(OUT_OUT);
        for i in 0..n {
            let x_in = audio[i];
            let exc = self.exciter_step(lp_alpha);
            let x = x_in + exc;

            // Run the parallel modal bank and sum.
            let mut acc = 0.0f32;
            for m in 0..NUM_MODES {
                let y = self.g[m] * x + self.c[m] * self.y1[m] - self.d[m] * self.y2[m];
                self.y2[m] = self.y1[m];
                self.y1[m] = y;
                acc += y;
            }
            out[i] = acc * MASTER_GAIN;
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Resonator);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Macro controls, in input-port order after `in`/`gate`: freq, structure, brightness, damping,
    /// position.
    #[derive(Clone, Copy)]
    struct Macros {
        freq: f32,
        structure: f32,
        brightness: f32,
        damping: f32,
        position: f32,
    }

    impl Default for Macros {
        fn default() -> Self {
            Self {
                freq: 220.0,
                structure: 0.0,
                brightness: 0.9,
                damping: 0.9,
                position: 0.3,
            }
        }
    }

    fn set_macros(d: &mut OpDriver, m: Macros) {
        d.set(IN_FREQ, m.freq)
            .set(IN_STRUCTURE, m.structure)
            .set(IN_BRIGHTNESS, m.brightness)
            .set(IN_DAMPING, m.damping)
            .set(IN_POSITION, m.position);
    }

    /// Ping the resonator (one mallet trigger at frame 0) and render `n` frames; return `out`.
    fn ping(n: usize, m: Macros) -> Vec<f32> {
        let mut d = OpDriver::for_type(Resonator::new(), SR);
        set_macros(&mut d, m);
        d.push(IN_GATE, 0, 1.0);
        d.render(n).output(OUT_OUT).to_vec()
    }

    /// Drive `input` through the `in` port as an audio effect (no gate) and return `out`.
    fn effect(input: &[f32], m: Macros) -> Vec<f32> {
        let mut d = OpDriver::for_type(Resonator::new(), SR);
        set_macros(&mut d, m);
        d.drive(IN_IN, input);
        d.render(input.len()).output(OUT_OUT).to_vec()
    }

    fn sine(f: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (std::f32::consts::TAU * f * i as f32 / SR).sin())
            .collect()
    }

    fn rms(buf: &[f32]) -> f32 {
        if buf.is_empty() {
            return 0.0;
        }
        (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt()
    }

    #[test]
    fn ping_rings_then_decays_to_silence() {
        // A struck resonator (moderate damping) sounds, then rings out — not a click, not a drone.
        let m = Macros {
            damping: 0.5,
            ..Macros::default()
        };
        let out = ping(96_000, m); // 2 s

        for (i, &s) in out.iter().enumerate() {
            assert!(s.is_finite(), "sample {i} not finite: {s}");
        }
        let early = rms(&out[2_000..12_000]); // ~0.04–0.25 s: the ring
        let late = rms(&out[84_000..96_000]); // ~1.75–2.0 s: rung out
        assert!(early > 1e-3, "ping should make sound, early rms {early}");
        assert!(
            late < early * 0.1,
            "ring should decay toward silence: early {early}, late {late}"
        );
    }

    #[test]
    fn higher_damping_rings_longer() {
        let long = ping(
            72_000,
            Macros {
                damping: 0.9,
                ..Macros::default()
            },
        );
        let short = ping(
            72_000,
            Macros {
                damping: 0.3,
                ..Macros::default()
            },
        );
        // Late in the render, the high-damping ping is still ringing while the low-damping one has
        // long since died.
        let late_long = rms(&long[60_000..72_000]);
        let late_short = rms(&short[60_000..72_000]);
        assert!(
            late_long > late_short * 4.0,
            "more damping should ring longer: long {late_long}, short {late_short}"
        );
    }

    #[test]
    fn unwired_is_silent() {
        // No gate, no input: nothing to excite the bank.
        let out = OpDriver::for_type(Resonator::new(), SR)
            .render(4_096)
            .output(OUT_OUT)
            .to_vec();
        assert!(
            out.iter().all(|&s| s == 0.0),
            "an un-excited resonator must be silent"
        );
    }

    #[test]
    fn resonates_at_the_fundamental() {
        // As an audio effect: a sine on the fundamental rings the bank far more than one sitting
        // between partials.
        let m = Macros::default(); // freq 220, harmonic, narrow
        let n = 72_000;
        let on = effect(&sine(220.0, n), m);
        let off = effect(&sine(311.0, n), m); // between 220 and 440, off every partial

        let on_rms = rms(&on[48_000..]); // after the narrow bank builds up
        let off_rms = rms(&off[48_000..]);
        assert!(
            on_rms > off_rms * 3.0,
            "should resonate at the fundamental: on {on_rms}, off {off_rms}"
        );
    }

    #[test]
    fn freq_sets_the_resonant_pitch() {
        // The same 220 Hz tone resonates strongly when the bank is tuned to 220, weakly when tuned
        // to 440 (where 220 is no partial).
        let n = 72_000;
        let tuned = effect(&sine(220.0, n), Macros::default());
        let detuned = effect(
            &sine(220.0, n),
            Macros {
                freq: 440.0,
                ..Macros::default()
            },
        );
        let tuned_rms = rms(&tuned[48_000..]);
        let detuned_rms = rms(&detuned[48_000..]);
        assert!(
            tuned_rms > detuned_rms * 3.0,
            "freq should set the resonant pitch: tuned {tuned_rms}, detuned {detuned_rms}"
        );
    }

    #[test]
    fn structure_shifts_the_partials_inharmonically() {
        // At structure 0 the 2nd partial sits exactly on 2·freq (440); cranking structure stretches
        // it away, so a 440 Hz tone no longer finds a partial there.
        let n = 72_000;
        let harmonic = effect(
            &sine(440.0, n),
            Macros {
                structure: 0.0,
                ..Macros::default()
            },
        );
        let stretched = effect(
            &sine(440.0, n),
            Macros {
                structure: 1.0,
                ..Macros::default()
            },
        );
        let h = rms(&harmonic[48_000..]);
        let s = rms(&stretched[48_000..]);
        assert!(
            h > s * 3.0,
            "structure should move the 2nd partial off 2·freq: harmonic {h}, stretched {s}"
        );
    }

    #[test]
    fn brightness_controls_high_partial_content() {
        // The 8th partial (1760 Hz) only resonates when brightness keeps the high modes alive.
        let n = 72_000;
        let tone = sine(1_760.0, n);
        let bright = effect(
            &tone,
            Macros {
                brightness: 0.9,
                ..Macros::default()
            },
        );
        let dark = effect(
            &tone,
            Macros {
                brightness: 0.2,
                ..Macros::default()
            },
        );
        let b = rms(&bright[48_000..]);
        let d = rms(&dark[48_000..]);
        assert!(
            b > d * 5.0,
            "brightness should keep high partials: bright {b}, dark {d}"
        );
    }

    #[test]
    fn held_gate_fires_once_not_repeatedly() {
        // Gate held high for the whole render (no off edge): the mallet fires once, so the ring
        // decays rather than being re-excited every block.
        let mut d = OpDriver::for_type(Resonator::new(), SR);
        set_macros(
            &mut d,
            Macros {
                damping: 0.5,
                ..Macros::default()
            },
        );
        d.push(IN_GATE, 0, 1.0); // high, and never lowered
        let out = d.render(72_000).output(OUT_OUT).to_vec();
        let early = rms(&out[2_000..12_000]);
        let late = rms(&out[60_000..72_000]);
        assert!(early > 1e-3, "the single ping should sound");
        assert!(
            late < early * 0.2,
            "a held gate must not keep re-triggering: early {early}, late {late}"
        );
    }

    #[test]
    fn stays_bounded_under_extremes() {
        // Loud broadband input, maximum ring and brightness, plus a ping — must not blow up.
        let n = 48_000;
        let loud: Vec<f32> = sine(220.0, n).iter().map(|s| s * 8.0).collect();
        let mut d = OpDriver::for_type(Resonator::new(), SR);
        set_macros(
            &mut d,
            Macros {
                brightness: 1.0,
                damping: 1.0,
                structure: 0.5,
                ..Macros::default()
            },
        );
        d.push(IN_GATE, 0, 1.0);
        d.drive(IN_IN, &loud);
        let out = d.render(n).output(OUT_OUT).to_vec();
        for (i, &s) in out.iter().enumerate() {
            assert!(s.is_finite(), "sample {i} not finite: {s}");
            assert!(s.abs() < 100.0, "sample {i} unbounded: {s}");
        }
    }

    #[test]
    fn ring_is_continuous_across_block_slices() {
        // One render of n must equal two back-to-back renders of n/2 sharing the operator: the bank
        // state and PRNG thread across the real block boundaries. n and the half are 128-aligned so
        // the block grids line up.
        let m = Macros {
            damping: 0.7,
            ..Macros::default()
        };
        let n = 8_192;
        let whole = ping(n, m);

        let mut split = OpDriver::for_type(Resonator::new(), SR);
        set_macros(&mut split, m);
        split.push(IN_GATE, 0, 1.0);
        let a = split.render(n / 2).output(OUT_OUT).to_vec();
        let b = split.render(n / 2).output(OUT_OUT).to_vec();

        for i in 0..n / 2 {
            assert!(
                (whole[i] - a[i]).abs() < 1e-4,
                "block 1 differs at {i}: {} vs {}",
                whole[i],
                a[i]
            );
            assert!(
                (whole[n / 2 + i] - b[i]).abs() < 1e-4,
                "block 2 differs at {i}: {} vs {}",
                whole[n / 2 + i],
                b[i]
            );
        }
    }

    #[test]
    fn spawned_resonator_starts_silent() {
        // Ring `a`, then spawn `b`: the fresh voice carries no ring and stays silent until pinged.
        let mut a = OpDriver::for_type(Resonator::new(), SR);
        set_macros(&mut a, Macros::default());
        a.push(IN_GATE, 0, 1.0);
        a.render(8_000);

        let out = a.spawn().render(4_000).output(OUT_OUT).to_vec();
        assert!(
            out.iter().all(|&s| s.abs() < 1e-6),
            "a spawned resonator must start silent (no inherited ring)"
        );
    }
}
