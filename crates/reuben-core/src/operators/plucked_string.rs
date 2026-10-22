//! Plucked string — a Karplus–Strong string, extended after Jaffe and Smith.
//!
//! One delay line a period long, closed through a first-order allpass that tunes the fractional
//! part of the period and a one-pole loss filter that sheds high partials faster than low ones. A
//! pluck adds one period of shaped noise into the line; the loop does the rest. The partials are
//! harmonic by construction, which is what sets it apart from the
//! [`Resonator`](crate::operators::Resonator)'s modal bank, and its cost is a few operations a
//! sample whatever its pitch or brightness.
//!
//! The controls are the Resonator's, less `structure`, so one stands in for the other:
//! - `freq` — the fundamental (Hz). Clamped to the line's floor (see
//!   [`PluckedString::with_lowest_freq`]) and to a sixth of the sample rate.
//! - `gate` — a rising edge across 0.5 plucks; the edge value is the velocity, so it spans
//!   (0.5, 1.0]. Level is linear in it.
//! - `brightness` — how much faster the upper partials die than the fundamental, and how bright
//!   the pluck's noise is. Dark is a thumb on nylon; bright is a pick on steel.
//! - `damping` — the fundamental's ring time, mapped as the Resonator maps it, so the two ring
//!   alike at the same setting.
//! - `position` — where along the string it is plucked, as a comb on the pluck: partials with a
//!   node there are not excited. Mapped as the Resonator maps it, to `[0.05, 0.5]` of the length.
//! - `in` — external excitation (bowing, breath), added into the loop and normalized so a
//!   sustained tone at the fundamental passes at unity.
//!
//! - output 0: `out` (`Buffer`) — the string.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use num_traits::Float;

use crate::config::AudioConfig;
use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::plan::PlanError;

// Single-source contract: one declaration -> IN_/OUT_/C_ consts + Descriptor, no drift.
crate::operator_contract!(PluckedString {
    type_name: "plucked_string",
    inputs: { in: f32_buffer,
              freq:       f32 { 20.0..=8000.0, default 220.0, "Hz", exp },
              gate:       f32 { 0.0..=1.0,     default 0.0,   "",   lin },
              brightness: f32 { 0.0..=1.0,     default 0.5,   "",   lin },
              damping:    f32 { 0.0..=1.0,     default 0.7,   "",   lin },
              position:   f32 { 0.0..=1.0,     default 0.3,   "",   lin } },
    outputs: { out: f32_buffer },
});

/// The lowest `freq` the line holds for [`PluckedString::new`]; the bottom of the `freq` range.
const LOWEST_FREQ: f32 = 20.0;
/// The highest `freq`, as a fraction of the sample rate. The loop needs at least one whole sample
/// of line besides what the allpass and loss filter delay, and the loss filter delays up to ~2.3.
const HIGHEST_FREQ_RATIO: f32 = 1.0 / 6.0;
/// The fundamental's decay time constant (seconds), `damping` 0 to 1, exponentially. The
/// Resonator's own range, so the two ring alike.
const T_MIN: f32 = 0.02;
const T_MAX: f32 = 8.0;
/// How much faster partial `k` decays than the fundamental: its rate is `1 + h·(k - 1)` times the
/// fundamental's, with `h = HF_DAMP_MAX·(1 - brightness)²`. 0 at `brightness` 1, where every partial
/// decays alike.
const HF_DAMP_MAX: f32 = 4.0;
/// The partial the loss filter is fitted at, besides the fundamental, where it is below Nyquist.
const FIT_PARTIAL: f32 = 8.0;
/// Loss-filter pole ceiling. Its phase delay, `~p / (1 - p)` samples, has to fit in a period.
const LOSS_POLE_MAX: f32 = 0.9;
/// Loop gain ceiling: a hair under 1, so no setting rings forever.
const LOOP_GAIN_MAX: f32 = 0.999_99;
/// The pluck's noise is low-passed at `freq` times 2 (dark) to 64 (bright).
const PLUCK_COLOUR_MIN: f32 = 2.0;
const PLUCK_COLOUR_MAX: f32 = 64.0;
/// Pluck level: a unit-velocity pluck peaks between about 0.4 (high and dark) and 0.85 (bright,
/// where the unfiltered noise has the higher crest factor), a full-level voice.
const PLUCK_GAIN: f32 = 0.38;
/// Fixed PRNG seed a fresh or spawned string starts from (xorshift cannot leave 0).
const SEED: u32 = 0x2545_F491;

/// `x` clamped to `0..=1`, or `nan` for a NaN: `f32::clamp` passes a NaN through.
fn unit(x: f32, nan: f32) -> f32 {
    if x.is_nan() { nan } else { x.clamp(0.0, 1.0) }
}

pub struct PluckedString {
    /// The string: the last `len` samples of the loop. Sized by `on_instantiate`.
    line: Vec<f32>,
    /// Write head into `line`.
    head: usize,
    /// Construction-time floor on `freq`; it sizes `line`.
    lowest_freq: f32,

    /// Whole samples of loop delay in `line`; the allpass and loss filter make up the rest.
    delay: usize,
    /// Allpass coefficient: `y = a·x + x1 - a·y1`.
    allpass: f32,
    /// Loss-filter pole: `y = (1 - p)·x + p·y1`.
    loss_pole: f32,
    /// Loop gain on top of the loss filter, chosen so the fundamental decays at `damping`'s rate.
    loop_gain: f32,
    /// `in`'s gain: one less the fundamental's per-period loop gain, which is unity at resonance.
    in_gain: f32,
    /// The pluck noise's lowpass pole, and the gain that holds its RMS whatever the pole.
    pluck_pole: f32,
    pluck_norm: f32,

    ap_x1: f32,
    ap_y1: f32,
    loss_y1: f32,

    /// Control values the coefficients were computed for; NaN forces the first compute.
    last_freq: f32,
    last_brightness: f32,
    last_damping: f32,
    last_sr: f32,

    rng: u32,
    prev_gate: bool,
}

impl Default for PluckedString {
    fn default() -> Self {
        Self::new()
    }
}

impl PluckedString {
    pub fn new() -> Self {
        Self::with_lowest_freq(LOWEST_FREQ)
    }

    /// A string whose line holds `freq` down to `lowest_freq`, clamped to the `freq` range; a NaN
    /// takes the floor. A lower `freq` plays at this one. The line costs
    /// `4 * (ceil(sample_rate / lowest_freq) + 2)` bytes of heap.
    pub fn with_lowest_freq(lowest_freq: f32) -> Self {
        let lowest_freq = if lowest_freq >= LOWEST_FREQ {
            lowest_freq.min(8000.0)
        } else {
            LOWEST_FREQ
        };
        Self {
            line: Vec::new(),
            head: 0,
            lowest_freq,
            delay: 1,
            allpass: 0.0,
            loss_pole: 0.0,
            loop_gain: 0.0,
            in_gain: 0.0,
            pluck_pole: 0.0,
            pluck_norm: 0.0,
            ap_x1: 0.0,
            ap_y1: 0.0,
            loss_y1: 0.0,
            last_freq: f32::NAN,
            last_brightness: f32::NAN,
            last_damping: f32::NAN,
            last_sr: f32::NAN,
            rng: SEED,
            prev_gate: false,
        }
    }

    fn line_len(&self, sample_rate: f32) -> usize {
        Float::ceil(sample_rate / self.lowest_freq) as usize + 2
    }

    #[inline]
    fn next_noise(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        ((x >> 8) as f32) * (1.0 / (1u32 << 24) as f32) * 2.0 - 1.0
    }

    /// Recompute the loop from the held controls. Pure given its args.
    ///
    /// The loop's delay at the fundamental is the line's whole samples, the allpass's fractional
    /// part, and the loss filter's phase delay there; the allpass is solved exactly for what is
    /// left, at the fundamental, so the pitch holds even where the filters' delays bend with
    /// frequency.
    fn recompute(&mut self, freq: f32, brightness: f32, damping: f32, sample_rate: f32) {
        use core::f32::consts::TAU;
        let highest = sample_rate * HIGHEST_FREQ_RATIO;
        let freq = if freq.is_nan() { 220.0 } else { freq }
            .max(self.lowest_freq)
            .min(highest);
        let brightness = unit(brightness, 0.5);
        let damping = unit(damping, 0.7);
        let w = TAU * freq / sample_rate;
        let period = sample_rate / freq;

        let tau = T_MIN * Float::powf(T_MAX / T_MIN, damping);
        let per_period = Float::exp(-1.0 / (tau * freq));

        // Fit the loss filter's slope to the decay wanted at `FIT_PARTIAL` against the
        // fundamental's: with `r` the squared ratio of their per-period gains,
        // `|H(w1)|² / |H(wk)|² = r` is a quadratic in the pole. Past what one pole can reach (about
        // `1/k`), take the steepest pole allowed.
        let hf_damp = HF_DAMP_MAX * (1.0 - brightness) * (1.0 - brightness);
        let k = Float::floor(FIT_PARTIAL.min(0.9 * core::f32::consts::PI / w));
        let p = if hf_damp > 0.0 && k >= 2.0 {
            let r = Float::exp(-2.0 * hf_damp * (k - 1.0) / (tau * freq));
            let b = Float::cos(w) - r * Float::cos(k * w);
            let disc = b * b - (1.0 - r) * (1.0 - r);
            if disc >= 0.0 {
                ((b - Float::sqrt(disc)) / (1.0 - r)).clamp(0.0, LOSS_POLE_MAX)
            } else {
                LOSS_POLE_MAX
            }
        } else {
            0.0
        };
        let (sin_w, cos_w) = (Float::sin(w), Float::cos(w));
        let loss_mag = (1.0 - p) / Float::sqrt(1.0 - 2.0 * p * cos_w + p * p);
        let loss_delay = Float::atan2(p * sin_w, 1.0 - p * cos_w) / w;

        // Whole samples, leaving the allpass 0.5..1.5, where a first-order allpass is well behaved.
        let rest = period - loss_delay;
        let max_delay = self.line.len().saturating_sub(1).max(1);
        let delay = (Float::floor(rest - 0.5) as usize).clamp(1, max_delay);
        let frac = rest - delay as f32;
        self.allpass = Float::sin(w * (1.0 - frac) / 2.0) / Float::sin(w * (1.0 + frac) / 2.0);
        self.delay = delay;
        self.loss_pole = p;

        self.loop_gain = (per_period / loss_mag).min(LOOP_GAIN_MAX);
        self.in_gain = 1.0 - self.loop_gain * loss_mag;

        let colour = PLUCK_COLOUR_MIN * Float::powf(PLUCK_COLOUR_MAX / PLUCK_COLOUR_MIN, brightness);
        let cutoff = (freq * colour).min(0.45 * sample_rate);
        let q = Float::exp(-TAU * cutoff / sample_rate);
        self.pluck_pole = q;
        // One-pole lowpassed white noise has `(1 - q) / (1 + q)` of the input's variance, and the
        // position comb doubles it.
        self.pluck_norm = Float::sqrt((1.0 + q) / (1.0 - q) / 2.0);
    }

    /// Adds one period of shaped noise to the samples the loop reads next.
    ///
    /// The position comb subtracts the burst from itself `m` samples later, circularly over the
    /// period, so a partial with a node at the plucking point cancels whole. The burst is generated
    /// twice from the same PRNG state rather than kept: a second buffer would be another line's
    /// worth of heap.
    fn pluck(&mut self, velocity: f32, position: f32) {
        let len = self.line.len();
        let n = self.delay;
        if len == 0 {
            return;
        }
        let pos = 0.05 + 0.45 * unit(position, 0.3);
        let m = (Float::round(pos * n as f32) as usize).clamp(1, n.saturating_sub(1).max(1));
        let amp = velocity * PLUCK_GAIN * self.pluck_norm;
        let q = self.pluck_pole;
        let start = (self.head + len - n) % len;
        let at = |k: usize| if start + k >= len { start + k - len } else { start + k };

        let seed = self.rng;
        let mut e = 0.0f32;
        for k in 0..n {
            e = (1.0 - q) * self.next_noise() + q * e;
            self.line[at(k)] += amp * e;
        }
        let after = self.rng;
        self.rng = seed;
        e = 0.0;
        for k in 0..n {
            e = (1.0 - q) * self.next_noise() + q * e;
            let j = if k + m >= n { k + m - n } else { k + m };
            self.line[at(j)] -= amp * e;
        }
        self.rng = after;
    }
}

impl Operator for PluckedString {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn on_instantiate(&mut self, config: &AudioConfig) -> Result<(), PlanError> {
        self.line = vec![0.0f32; self.line_len(config.sample_rate)];
        self.head = 0;
        self.last_sr = f32::NAN;
        Ok(())
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Sized by `on_instantiate`, so under a Plan this never fires; it guards a box that is
        // processed without being instantiated.
        let cap = self.line_len(sample_rate);
        if self.line.len() != cap {
            self.line = vec![0.0f32; cap];
            self.head = 0;
            self.last_sr = f32::NAN;
        }

        let freq = io.read(IN_FREQ);
        let brightness = io.read(IN_BRIGHTNESS);
        let damping = io.read(IN_DAMPING);
        // `!=` is true for a NaN, so a NaN control recomputes every block; `recompute` maps it to
        // the port's default.
        if freq != self.last_freq
            || brightness != self.last_brightness
            || damping != self.last_damping
            || sample_rate != self.last_sr
        {
            self.recompute(freq, brightness, damping, sample_rate);
            self.last_freq = freq;
            self.last_brightness = brightness;
            self.last_damping = damping;
            self.last_sr = sample_rate;
        }

        // The engine block-slices at each `gate` change, so frame 0 of this slice is the edge.
        let gate = io.read(IN_GATE);
        let gate_hi = gate > 0.5;
        if gate_hi && !self.prev_gate {
            self.pluck(gate.min(1.0), io.read(IN_POSITION));
        }
        self.prev_gate = gate_hi;

        let len = self.line.len();
        let mut read = (self.head + len - self.delay) % len;
        let mut head = self.head;
        let (a, p, g, g_in) = (self.allpass, self.loss_pole, self.loop_gain, self.in_gain);
        let (mut ap_x1, mut ap_y1, mut loss_y1) = (self.ap_x1, self.ap_y1, self.loss_y1);
        let audio = io.read(IN_IN);
        let out = io.write(OUT_OUT);
        for i in 0..n {
            let d = self.line[read];
            let ap = a * d + ap_x1 - a * ap_y1;
            ap_x1 = d;
            ap_y1 = ap;
            loss_y1 = (1.0 - p) * ap + p * loss_y1;
            let y = g * loss_y1 + g_in * audio[i];
            self.line[head] = y;
            out[i] = y;
            read += 1;
            if read == len {
                read = 0;
            }
            head += 1;
            if head == len {
                head = 0;
            }
        }
        self.head = head;
        self.ap_x1 = ap_x1;
        self.ap_y1 = ap_y1;
        self.loss_y1 = loss_y1;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::with_lowest_freq(self.lowest_freq))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    #[derive(Clone, Copy)]
    struct Pluck {
        freq: f32,
        brightness: f32,
        damping: f32,
        position: f32,
        velocity: f32,
    }

    impl Default for Pluck {
        fn default() -> Self {
            Self {
                freq: 220.0,
                brightness: 0.5,
                damping: 0.8,
                position: 0.3,
                velocity: 1.0,
            }
        }
    }

    fn driver(op: PluckedString, p: Pluck) -> OpDriver {
        let mut d = OpDriver::for_type(op, SR);
        d.set(IN_FREQ, p.freq)
            .set(IN_BRIGHTNESS, p.brightness)
            .set(IN_DAMPING, p.damping)
            .set(IN_POSITION, p.position)
            .set(IN_GATE, p.velocity);
        d
    }

    fn pluck(p: Pluck, secs: f32) -> Vec<f32> {
        driver(PluckedString::new(), p)
            .render((secs * SR) as usize)
            .output(OUT_OUT)
            .to_vec()
    }

    fn span(x: &[f32], from: f32, to: f32) -> &[f32] {
        &x[(from * SR) as usize..((to * SR) as usize).min(x.len())]
    }

    fn peak(x: &[f32]) -> f32 {
        x.iter().fold(0.0f32, |a, s| a.max(s.abs()))
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|s| s * s).sum::<f32>() / x.len() as f32).sqrt()
    }

    fn hann(x: &[f32]) -> Vec<f64> {
        let n = x.len();
        x.iter()
            .enumerate()
            .map(|(i, &v)| {
                v as f64 * (0.5 - 0.5 * (core::f64::consts::TAU * i as f64 / (n - 1) as f64).cos())
            })
            .collect()
    }

    /// Magnitude of the DFT of an already-windowed `x` at `freq`, by Goertzel.
    fn goertzel(x: &[f64], freq: f32) -> f64 {
        let coeff = 2.0 * (core::f64::consts::TAU * freq as f64 / SR as f64).cos();
        let (mut s1, mut s2) = (0.0f64, 0.0f64);
        for &v in x {
            let s0 = v + coeff * s1 - s2;
            s2 = s1;
            s1 = s0;
        }
        (s1 * s1 + s2 * s2 - coeff * s1 * s2).max(0.0).sqrt() / x.len() as f64
    }

    /// Magnitude of the Hann-windowed DFT of `x` at `freq`.
    fn bin(x: &[f32], freq: f32) -> f64 {
        goertzel(&hann(x), freq)
    }

    /// The frequency of the strongest component within ±50 cents of `near`, to 0.1 cent.
    fn peak_hz(x: &[f32], near: f32) -> f32 {
        let x = hann(x);
        let strongest = |centre: f32, steps: i32, step_cents: f32| {
            (-steps..=steps)
                .map(|c| centre * 2f32.powf(c as f32 * step_cents / 1200.0))
                .max_by(|a, b| goertzel(&x, *a).total_cmp(&goertzel(&x, *b)))
                .expect("a non-empty search")
        };
        let coarse = strongest(near, 25, 2.0);
        strongest(coarse, 20, 0.1)
    }

    fn cents(f: f32, reference: f32) -> f32 {
        1200.0 * (f / reference).log2()
    }

    #[test]
    fn it_is_silent_until_plucked() {
        let out = pluck(
            Pluck {
                velocity: 0.0,
                ..Pluck::default()
            },
            0.5,
        );
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_pluck_rings_at_its_freq() {
        for freq in [82.41, 110.0, 220.0, 523.25, 1046.5, 2000.0] {
            for brightness in [0.1, 0.9] {
                let out = pluck(
                    Pluck {
                        freq,
                        brightness,
                        damping: 0.9,
                        ..Pluck::default()
                    },
                    1.5,
                );
                let got = peak_hz(span(&out, 0.5, 1.5), freq);
                let off = cents(got, freq);
                assert!(
                    off.abs() < 3.0,
                    "{freq} Hz at brightness {brightness} rings at {got} Hz, {off:.1} cents off"
                );
            }
        }
    }

    #[test]
    fn its_partials_are_harmonic() {
        let out = pluck(
            Pluck {
                brightness: 0.9,
                damping: 0.9,
                ..Pluck::default()
            },
            1.0,
        );
        let x = span(&out, 0.2, 1.0);
        for k in 2..=6 {
            let want = 220.0 * k as f32;
            let off = cents(peak_hz(x, want), want);
            assert!(off.abs() < 5.0, "partial {k} is {off:.1} cents off");
        }
    }

    #[test]
    fn damping_sets_the_fundamentals_ring_time() {
        for damping in [0.5f32, 0.8] {
            let out = pluck(
                Pluck {
                    damping,
                    ..Pluck::default()
                },
                3.0,
            );
            let want = T_MIN * (T_MAX / T_MIN).powf(damping);
            let (t0, dt) = (0.3, (want * 0.8).min(1.0));
            let a0 = bin(span(&out, t0, t0 + 0.2), 220.0);
            let a1 = bin(span(&out, t0 + dt, t0 + dt + 0.2), 220.0);
            let got = dt / (a0 / a1).ln() as f32;
            assert!(
                (got / want - 1.0).abs() < 0.15,
                "damping {damping}: rings {got:.3} s, mapped {want:.3} s"
            );
        }
    }

    #[test]
    fn a_darker_string_loses_its_upper_partials_sooner() {
        let fifth = |brightness| {
            let out = pluck(
                Pluck {
                    brightness,
                    ..Pluck::default()
                },
                1.0,
            );
            let x = span(&out, 0.4, 0.8);
            bin(x, 1100.0) / bin(x, 220.0)
        };
        let (dark, bright) = (fifth(0.1), fifth(0.9));
        assert!(dark * 10.0 < bright, "dark {dark:.5}, bright {bright:.5}");
    }

    #[test]
    fn plucking_the_middle_cancels_the_even_partials() {
        let second = |position| {
            let out = pluck(
                Pluck {
                    position,
                    brightness: 0.9,
                    ..Pluck::default()
                },
                0.5,
            );
            let x = span(&out, 0.05, 0.5);
            bin(x, 440.0) / bin(x, 220.0)
        };
        let (middle, near_the_end) = (second(1.0), second(0.2));
        assert!(
            middle * 10.0 < near_the_end,
            "middle {middle:.5}, near the end {near_the_end:.5}"
        );
    }

    #[test]
    fn velocity_scales_the_level() {
        let at = |velocity| {
            peak(&pluck(
                Pluck {
                    velocity,
                    ..Pluck::default()
                },
                0.5,
            ))
        };
        let ratio = at(0.6) / at(1.0);
        assert!((ratio - 0.6).abs() < 0.01, "0.6 velocity is {ratio} of full");
    }

    #[test]
    fn a_unit_pluck_is_a_full_level_voice_at_any_pitch_and_brightness() {
        for freq in [82.41, 220.0, 880.0, 2000.0] {
            for brightness in [0.0, 0.5, 1.0] {
                let level = peak(&pluck(
                    Pluck {
                        freq,
                        brightness,
                        ..Pluck::default()
                    },
                    0.3,
                ));
                assert!(
                    (0.3..=0.9).contains(&level),
                    "{freq} Hz at brightness {brightness} peaks at {level}"
                );
            }
        }
    }

    #[test]
    fn a_held_gate_plucks_once_and_a_new_edge_plucks_again() {
        let mut d = driver(PluckedString::new(), Pluck::default());
        let first = peak(d.render((0.3 * SR) as usize).output(OUT_OUT));
        let held = rms(d.render((0.3 * SR) as usize).output(OUT_OUT));
        let decayed = rms(d.set(IN_GATE, 0.0).render(1024).output(OUT_OUT));
        let again = peak(d.set(IN_GATE, 1.0).render((0.3 * SR) as usize).output(OUT_OUT));
        assert!(held < first * 0.5, "a held gate kept plucking: {held} after {first}");
        assert!(again > decayed * 2.0, "a new edge did not pluck: {again} after {decayed}");
    }

    #[test]
    fn a_sustained_tone_at_the_fundamental_passes_at_unity() {
        let n = (4.0 * SR) as usize;
        let sine: Vec<f32> = (0..n)
            .map(|i| (core::f32::consts::TAU * 220.0 * i as f32 / SR).sin())
            .collect();
        let out = driver(
            PluckedString::new(),
            Pluck {
                damping: 0.5,
                velocity: 0.0,
                ..Pluck::default()
            },
        )
        .drive(IN_IN, &sine)
        .render(n)
        .output(OUT_OUT)
        .to_vec();
        let gain_db = 20.0 * (rms(span(&out, 3.0, 4.0)) / core::f32::consts::FRAC_1_SQRT_2).log10();
        assert!(gain_db.abs() < 1.5, "a tone at the fundamental passes at {gain_db:.2} dB");
    }

    #[test]
    fn the_longest_brightest_ring_never_grows() {
        for freq in [20.0, 220.0, 4000.0, 8000.0] {
            let out = pluck(
                Pluck {
                    freq,
                    brightness: 1.0,
                    damping: 1.0,
                    ..Pluck::default()
                },
                10.0,
            );
            assert!(out.iter().all(|s| s.is_finite()));
            let (start, end) = (peak(span(&out, 0.0, 1.0)), peak(span(&out, 9.0, 10.0)));
            assert!(end <= start, "{freq} Hz grew from {start} to {end}");
        }
    }

    #[test]
    fn a_nan_control_is_not_a_poisoned_string() {
        let out = pluck(
            Pluck {
                freq: f32::NAN,
                brightness: f32::NAN,
                damping: f32::NAN,
                position: f32::NAN,
                velocity: 1.0,
            },
            0.5,
        );
        assert!(out.iter().all(|s| s.is_finite()));
        assert!(peak(&out) > 0.0);
    }

    #[test]
    fn a_freq_under_the_lines_floor_rings_at_the_floor() {
        let out = driver(
            PluckedString::with_lowest_freq(100.0),
            Pluck {
                freq: 50.0,
                damping: 0.9,
                ..Pluck::default()
            },
        )
        .render((1.0 * SR) as usize)
        .output(OUT_OUT)
        .to_vec();
        let off = cents(peak_hz(span(&out, 0.3, 1.0), 100.0), 100.0);
        assert!(off.abs() < 3.0, "rings {off:.1} cents from 100 Hz");
    }

    #[test]
    fn retuning_a_ringing_string_moves_its_pitch() {
        let mut d = driver(
            PluckedString::new(),
            Pluck {
                damping: 0.9,
                ..Pluck::default()
            },
        );
        d.render((0.3 * SR) as usize);
        let out = d.set(IN_FREQ, 330.0).render(SR as usize).output(OUT_OUT).to_vec();
        assert!(out.iter().all(|s| s.is_finite()));
        let off = cents(peak_hz(span(&out, 0.2, 1.0), 330.0), 330.0);
        assert!(off.abs() < 3.0, "retuned string is {off:.1} cents from 330 Hz");
    }

    #[test]
    fn a_spawned_string_starts_silent() {
        let mut d = driver(PluckedString::new(), Pluck::default());
        d.render(4096);
        let out = d
            .spawn()
            .set(IN_GATE, 0.0)
            .render(4096)
            .output(OUT_OUT)
            .to_vec();
        assert!(out.iter().all(|s| *s == 0.0));
    }
}
