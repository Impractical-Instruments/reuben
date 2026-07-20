//! Resonator — a modal resonator (Mutable Instruments *Rings*-inspired).
//!
//! A bank of `NUM_MODES` tuned two-pole resonators ("modes") excited in parallel by two paths: an
//! **external input** (`in`, the audio-effect / "excite it with anything" path) and an **internal
//! mallet** — a short brightness-filtered noise burst fired by a rising edge on `gate` (the ping /
//! strum trigger). Output is **pure wet**: the ringing bank only, so dry/wet blending (if wanted) is
//! a downstream `mul`/`add` patch.
//!
//! The two paths enter each mode through **different gains**, because a two-pole resonator's gain
//! depends on what you feed it — sustained audio is normalized against its *resonant* gain, a struck
//! ping against its *impulse* response (see `recompute`). One shared gain cannot serve both: it
//! makes the ping's level slide around with `damping` and `freq`, which no downstream makeup gain
//! can undo. A unit-velocity ping is a full-level voice at any pitch and any ring time.
//!
//! It is authored single-Voice: one mono stream, hosted by the Voicer via the standard
//! `freq`/`gate` voice interface. So the existing `strum` op → Voicer → per-voice `gate` plucks it
//! for free, and a note-on simply pings it at the voice's `freq`.
//!
//! Macro controls map the *Rings* panel (all held `f32` Values — read once per
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
//! - input 2: `gate` (`Float`) — rising edge (crossing the engine's 0.5 on-threshold, as `envelope`
//!   uses) fires the mallet; the edge value scales it, so velocity spans (0.5, 1.0]. The `voicer`
//!   drives this with a plain 1.0, so a hosted voice always strikes at full velocity.
//! - input 3: `structure` (`Float`) — partial inharmonicity 0..1.
//! - input 4: `brightness` (`Float`) — spectral tilt 0..1.
//! - input 5: `damping` (`Float`) — ring time 0..1.
//! - input 6: `position` (`Float`) — excitation comb 0..1.
//! - output 0: `out` (`Buffer`) — the pure-wet resonator output.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract: one declaration -> IN_/OUT_ consts + Descriptor.
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
/// Pole-radius ceiling. Must stay far enough below 1 that `1 - r²` keeps usable f32 precision, yet
/// high enough that `T_MAX` is actually reachable — `r = exp(-1/(T_MAX·sr))` is 0.9999974 at 48 kHz
/// and 0.9999993 at 192 kHz, so a tighter ceiling would silently cap the ring time.
const R_MAX: f32 = 0.999_999_5;
/// Mallet contact time (seconds) as `BURST_K / √freq` — a shorter, stiffer strike on a shorter
/// string. It has to scale with pitch, and this exponent is the one that holds the ping level flat:
/// a mode draws energy from the burst both by its *length* (more noise samples) and by the *cycles*
/// it sees within it (coherent buildup), and those pull in opposite directions. A fixed contact time
/// tilts the level ~7 dB toward the treble; a fully period-scaled one (`∝ 1/freq`) tilts it ~6 dB
/// toward the bass. `1/√freq` is the balance point — measured flat within ~3 dB from 110 Hz to
/// 1760 Hz. `K` puts a 440 Hz strike at ~1.5 ms.
const BURST_K: f32 = 0.031;
/// Contact time is clamped to a physical range: never a sub-sample click, never a smear.
const BURST_MIN_SECS: f32 = 0.0005;
const BURST_MAX_SECS: f32 = 0.008;
/// Drive level of the mallet burst into the bank.
const EXC_GAIN: f32 = 4.0;
/// Output trim. With the mallet path impulse-normalized this is an honest level knob — it sets what
/// a unit-velocity ping peaks at (~0.6, i.e. a full-level voice), the same at every pitch and ring
/// time, rather than compensating for a gain that slides around underneath it.
const MASTER_GAIN: f32 = 0.08;
/// Fixed deterministic PRNG seed a fresh / spawned Resonator starts from (xorshift can't leave 0).
const SEED: u32 = 0x2545_F491;

pub struct Resonator {
    /// Per-mode two-pole resonator coefficients (recomputed only when a control changes).
    /// `y[n] = g·x[n] + g_exc·e[n] + c·y[n-1] - d·y[n-2]`.
    c: [f32; NUM_MODES],
    d: [f32; NUM_MODES],
    /// Input gain for the **external** `in` path — resonant-gain normalized (see `recompute`).
    g: [f32; NUM_MODES],
    /// Input gain for the **internal mallet** path — impulse normalized (see `recompute`).
    g_exc: [f32; NUM_MODES],
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
            g_exc: [0.0; NUM_MODES],
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
    ///
    /// The bank has two excitation paths, and they need **different** input gains, because a
    /// two-pole resonator's gain depends on what you feed it:
    /// - Its *steady-state* gain at resonance is `g / (1 - r²)`, so the external `in` path takes
    ///   `g = (1 - r²)·amp`. That normalizes a sustained tone to unity and is what keeps the effect
    ///   path bounded as `r → 1` (a long ring is a very high-Q filter).
    /// - Its *impulse* response peaks at `g / sin(w)`, so the mallet path takes `g_exc = sin(w)·amp`
    ///   — a unit-peak ping whatever the mode's decay or pitch.
    ///
    /// Sharing one gain across both is the trap: normalize for the sustained case and every ping
    /// comes out scaled by `(1 - r²)`, which is `1e-4` at long decays. The ring gets quieter the
    /// longer it rings, and higher notes get quieter than low ones — a level that slides around with
    /// `damping` and `freq` and can't be rescued by a fixed makeup gain downstream.
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
                self.g_exc[i] = 0.0;
                continue;
            }
            let w = std::f32::consts::TAU * f / sample_rate;
            let t = t0 / (1.0 + HF_DAMP * i as f32);
            let r = (-1.0 / (t * sample_rate)).exp().min(R_MAX);
            let comb = (std::f32::consts::PI * n * pos).sin().abs();
            let amp = brightness.powi(i as i32) * comb;
            self.c[i] = 2.0 * r * w.cos();
            self.d[i] = r * r;
            // Sustained input: (1 - r²) normalizes the resonant peak, bounding the bank as r → 1.
            self.g[i] = (1.0 - r * r) * amp;
            // Struck input: sin(w) cancels the 1/sin(w) in the impulse response, so a ping peaks at
            // `amp` regardless of how long the mode rings or how high it is tuned.
            self.g_exc[i] = w.sin() * amp;
        }
    }

    /// One sample of the modal bank: advance every mode and sum. `EXCITED` is a const so the mallet
    /// term monomorphizes away entirely while the bank is merely ringing — which is nearly all of
    /// the time, since a burst is ~3 ms of a multi-second ring. Keeps the hot steady-state loop at
    /// the same arithmetic it has always done (the micro bench gates this at 10%).
    #[inline(always)]
    fn bank_step<const EXCITED: bool>(&mut self, x_in: f32, exc: f32) -> f32 {
        let mut acc = 0.0f32;
        for m in 0..NUM_MODES {
            let drive = if EXCITED {
                self.g[m] * x_in + self.g_exc[m] * exc
            } else {
                self.g[m] * x_in
            };
            let y = drive + self.c[m] * self.y1[m] - self.d[m] * self.y2[m];
            self.y2[m] = self.y1[m];
            self.y1[m] = y;
            acc += y;
        }
        acc
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

        // Held controls: one read each, constant for this block-slice.
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
            let contact = (BURST_K / freq.max(1.0).sqrt()).clamp(BURST_MIN_SECS, BURST_MAX_SECS);
            self.burst_len = ((contact * sample_rate) as i32).max(1);
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
            // The two excitations enter through different per-mode gains (see `recompute`), so they
            // cannot be summed into one drive term. Idle mallet -> take the cheap bank.
            let acc = if exc != 0.0 {
                self.bank_step::<true>(x_in, exc)
            } else {
                self.bank_step::<false>(x_in, 0.0)
            };
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

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |a, &b| a.max(b.abs()))
    }

    /// The operator's descriptor defaults, which is what an unconfigured `resonator` node plays at.
    fn defaults() -> Macros {
        Macros {
            freq: 440.0,
            structure: 0.25,
            brightness: 0.5,
            damping: 0.7,
            position: 0.3,
        }
    }

    /// The level contract, and the reason `g_exc` exists: a unit-velocity ping is a *usable voice*
    /// straight out of the operator. Before the mallet path got its own impulse-normalized gain this
    /// peaked at 0.007 — 43 dB down, which read as "the resonator is broken" at the instrument.
    #[test]
    fn a_ping_at_the_defaults_is_a_full_level_voice() {
        let p = peak(&ping(96_000, defaults()));
        assert!(
            (0.25..=1.0).contains(&p),
            "a unit-velocity ping should land near full scale, got {p}"
        );
    }

    /// `damping` is a *decay-time* control, not a volume control. It used to be both: the shared
    /// (1 - r²) mode gain scaled the ping by the very quantity that sets the ring length, so a long
    /// ring came out 22 dB quieter than a short one.
    #[test]
    fn ping_level_is_independent_of_damping() {
        let levels: Vec<f32> = [0.1, 0.3, 0.5, 0.7, 0.9]
            .iter()
            .map(|&damping| {
                peak(&ping(
                    96_000,
                    Macros {
                        damping,
                        ..defaults()
                    },
                ))
            })
            .collect();
        let lo = levels.iter().copied().fold(f32::MAX, f32::min);
        let hi = levels.iter().copied().fold(0.0f32, f32::max);
        assert!(
            hi < lo * 2.0,
            "damping must not act as a volume knob (within 6 dB): {levels:?}"
        );
    }

    /// Every string of the harp should speak at the same level — a glissando must not fade out as it
    /// climbs. Guards both the `sin(w)` mode normalization and the `1/√freq` contact time.
    #[test]
    fn ping_level_is_independent_of_pitch() {
        let levels: Vec<f32> = [110.0, 220.0, 440.0, 880.0, 1760.0]
            .iter()
            .map(|&freq| peak(&ping(96_000, Macros { freq, ..defaults() })))
            .collect();
        let lo = levels.iter().copied().fold(f32::MAX, f32::min);
        let hi = levels.iter().copied().fold(0.0f32, f32::max);
        assert!(
            hi < lo * 2.0,
            "ping level must hold across the pitch range (within 6 dB): {levels:?}"
        );
    }

    /// Velocity (the gate's edge value) is the one thing that *should* scale the ping. Note the
    /// usable range is (0.5, 1.0]: `gate` must clear the engine's 0.5 on-threshold to count as an
    /// edge at all, so 0.6 is the softest strike, not 0.0.
    #[test]
    fn velocity_scales_the_ping() {
        let mut soft = OpDriver::for_type(Resonator::new(), SR);
        set_macros(&mut soft, defaults());
        soft.push(IN_GATE, 0, 0.6);
        let soft = peak(soft.render(48_000).output(OUT_OUT));

        let loud = peak(&ping(48_000, defaults()));
        let ratio = loud / soft;
        assert!(
            (1.5..=1.8).contains(&ratio),
            "a 0.6-velocity ping should be ~0.6x a full one, ratio {ratio}"
        );
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

    /// The top of the `damping` range must keep doing something. The old pole-radius ceiling
    /// (0.99995) pinned the fundamental's decay at ~0.42 s, so every value above ~0.52 produced a
    /// bit-identical ring and the documented `T_MAX` of 8 s was unreachable.
    #[test]
    fn damping_keeps_lengthening_at_the_top_of_its_range() {
        let mid = ping(
            96_000,
            Macros {
                damping: 0.65,
                ..defaults()
            },
        );
        let full = ping(
            96_000,
            Macros {
                damping: 1.0,
                ..defaults()
            },
        );
        let late_mid = rms(&mid[84_000..96_000]);
        let late_full = rms(&full[84_000..96_000]);
        assert!(
            late_full > late_mid * 2.0,
            "damping 1.0 must ring longer than 0.65: mid {late_mid}, full {late_full}"
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
