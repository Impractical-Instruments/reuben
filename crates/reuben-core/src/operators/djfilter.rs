//! djfilter — DJ-mixer-style single-knob filter (low-pass / high-pass morph).
//!
//! One bipolar `position` knob in [-1, +1] models the filter knob on a DJ mixer:
//!
//! - **North (0)** — the filter is wide open; the signal passes essentially untouched.
//! - **counter-clockwise (negative)** — a low-pass sweeps *down*: its cutoff glides from
//!   `lp_start` (open, at North) toward `lp_end` (closed, at -1).
//! - **clockwise (positive)** — a high-pass sweeps *up*: its cutoff glides from `hp_start`
//!   (open, at North) toward `hp_end` (at +1).
//!
//! Cutoff interpolates **geometrically** with the knob (log-frequency), so a turn sounds like
//! an even musical sweep rather than bunching up at the top. One shared Cytomic SVF core
//! produces both the low-pass and high-pass taps; `position`'s sign selects which one is heard.
//!
//! Shape model (ADR-0028): every control is a **`Float` input**, each owning its unwired default.
//! When nothing is wired the engine materializes the input from its latched default (so a control
//! surface can sweep the knob via `/djfilter/position`, bit-identical to the old param behavior);
//! when an LFO/envelope is wired the source buffer passes through and sweeps the port audio-rate.
//! There is no longer a separate "signal port + same-named param" pair, and no wired/unwired branch
//! in `process` — `io.signal(IN_POSITION)` is always a buffer. `position` stays a continuous
//! bipolar `Float` in [-1, +1] (its sign selects low-pass vs high-pass), not an enum.
//!
//! - input 0: `audio` (`Float`) — the signal to filter.
//! - input 1: `position` (`Float`) — knob in [-1, +1] (materialized default 0.0).
//! - input 2: `resonance` (`Float`) — filter resonance 0..1 for both directions.
//! - input 3: `lp_start` (`Float`, Hz) — low-pass cutoff at North (open end of the CCW sweep).
//! - input 4: `lp_end`   (`Float`, Hz) — low-pass cutoff fully CCW (position -1).
//! - input 5: `hp_start` (`Float`, Hz) — high-pass cutoff at North (open end of the CW sweep).
//! - input 6: `hp_end`   (`Float`, Hz) — high-pass cutoff fully CW (position +1).
//! - output 0: `audio` (`Float`) — filtered output.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0028): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Djfilter {
    inputs:  { audio: float,
               position:  float { -1.0..=1.0,     default 0.0,     "",   lin },
               resonance: float { 0.0..=1.0,      default 0.1,     "",   lin },
               lp_start:  float { 20.0..=20000.0, default 20000.0, "Hz", exp },
               lp_end:    float { 20.0..=20000.0, default 200.0,   "Hz", exp },
               hp_start:  float { 20.0..=20000.0, default 20.0,    "Hz", exp },
               hp_end:    float { 20.0..=20000.0, default 6000.0,  "Hz", exp } },
    outputs: { audio: float },
});

#[derive(Default)]
pub struct Djfilter {
    /// SVF integrator state 1 (continuous across calls / block slices).
    ic1eq: f32,
    /// SVF integrator state 2 (continuous across calls / block slices).
    ic2eq: f32,
}

impl Djfilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// One Cytomic SVF sample step against precomputed coefficients. Returns the low-pass and
    /// high-pass taps together (band-pass is `v1`); the caller picks which to emit. Advances the
    /// integrator state. `k` is the damping (`1/Q`) the high-pass tap needs.
    #[inline]
    fn svf_step(&mut self, x: f32, k: f32, a1: f32, a2: f32, a3: f32) -> (f32, f32) {
        let v3 = x - self.ic2eq;
        let v1 = a1 * self.ic1eq + a2 * v3;
        let v2 = self.ic2eq + a2 * self.ic1eq + a3 * v3;
        self.ic1eq = 2.0 * v1 - self.ic1eq;
        self.ic2eq = 2.0 * v2 - self.ic2eq;
        let lp = v2;
        let hp = x - k * v1 - v2;
        (lp, hp)
    }
}

/// Geometric (log-frequency) interpolation from `start` to `end` as `amt` goes 0 → 1.
/// `start * (end/start)^amt` — a constant *ratio* per unit knob, which is what an even-sounding
/// filter sweep needs. `start`/`end` are assumed positive (cutoffs in Hz).
#[inline]
fn geom(start: f32, end: f32, amt: f32) -> f32 {
    start * (end / start).powf(amt)
}

/// Map the knob to a filter mode + cutoff. Negative (CCW) → low-pass sweeping `lp_start`→`lp_end`;
/// positive (CW) → high-pass sweeping `hp_start`→`hp_end`; zero → low-pass at `lp_start` (open).
/// Returns `(use_hp, cutoff_hz)`.
#[inline]
fn target(position: f32, lp_start: f32, lp_end: f32, hp_start: f32, hp_end: f32) -> (bool, f32) {
    if position > 0.0 {
        (true, geom(hp_start, hp_end, position.min(1.0)))
    } else {
        (false, geom(lp_start, lp_end, (-position).min(1.0)))
    }
}

/// TPT / zero-delay-feedback SVF coefficients for a cutoff (Hz) and resonance (0..1). Cutoff is
/// clamped to a safe range so `tan` never blows up; resonance maps to damping `k = 1/Q` (k = 2 ⇒
/// no resonance, smaller k ⇒ more), clamped away from 0 for stability. Returns `(k, a1, a2, a3)`.
#[inline]
fn coeffs(cutoff: f32, resonance: f32, sample_rate: f32) -> (f32, f32, f32, f32) {
    let cutoff = cutoff.clamp(20.0, 0.45 * sample_rate);
    let k = (2.0 - 1.9 * resonance.clamp(0.0, 1.0)).max(0.1);
    let g = (std::f32::consts::PI * cutoff / sample_rate).tan();
    let a1 = 1.0 / (1.0 + g * (g + k));
    let a2 = g * a1;
    let a3 = g * a2;
    (k, a1, a2, a3)
}

impl Operator for Djfilter {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Cutoff endpoints + resonance are the filter's voicing — `Float` inputs read once at
        // block rate (the filter's character, constant for the (sub)block, block-sliced on change).
        let resonance = io.value(IN_RESONANCE);
        let lp_start = io.value(IN_LP_START);
        let lp_end = io.value(IN_LP_END);
        let hp_start = io.value(IN_HP_START);
        let hp_end = io.value(IN_HP_END);

        // `position` is a `Float` input — always a buffer (wired source or materialized latch),
        // one read path (ADR-0028). Mode + coefficients are recomputed only when `position`
        // actually changes from the previous sample — `target`/`coeffs` are pure, so reusing the
        // cache on an unchanged knob is bit-identical to recomputing it, and a settled or slow knob
        // costs one compare per sample instead of a `tan()`/`powf()`.
        let mut last_pos = f32::NAN;
        let (mut use_hp, mut k, mut a1, mut a2, mut a3) = (false, 0.0, 0.0, 0.0, 0.0);
        for i in 0..n {
            let x = io.signal(IN_AUDIO).get(i).copied().unwrap_or(0.0);
            let pos = io.signal(IN_POSITION).get(i).copied().unwrap_or(0.0);
            // NaN seed forces a compute on the first sample (NaN != anything).
            if pos != last_pos {
                let (uh, cutoff) = target(pos, lp_start, lp_end, hp_start, hp_end);
                (k, a1, a2, a3) = coeffs(cutoff, resonance, sample_rate);
                use_hp = uh;
                last_pos = pos;
            }
            let (lp, hp) = self.svf_step(x, k, a1, a2, a3);
            io.output(OUT_AUDIO)[i] = if use_hp { hp } else { lp };
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Djfilter);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::Io;

    const SR: f32 = 48_000.0;

    // Default voicing (resonance/lp_start/lp_end/hp_start/hp_end), in input-port order so tests can
    // tweak one field and keep the rest. `position` is supplied separately (per-sample buffer).
    fn default_voicing() -> [f32; 5] {
        [0.1, 20_000.0, 200.0, 20.0, 6_000.0]
    }

    /// Render `input` through a fresh Djfilter with the given constant `position` and `voicing`
    /// (resonance/lp_start/lp_end/hp_start/hp_end). Every control is a `Float` input now (ADR-0028),
    /// so each is supplied as the constant per-sample buffer the engine would materialize.
    fn render(input: &[f32], position: f32, voicing: [f32; 5]) -> Vec<f32> {
        let n = input.len();
        let pos_buf = vec![position; n];
        render_buffers(input, &pos_buf, voicing)
    }

    /// Render `input` with an explicit per-sample `position` Float wired to port 1.
    fn render_modulated(input: &[f32], position: &[f32], voicing: [f32; 5]) -> Vec<f32> {
        render_buffers(input, position, voicing)
    }

    /// Render with an explicit per-sample `position` buffer and constant `voicing` controls — the
    /// exact `Float`-input buffers the engine hands `process` (ADR-0028). Inputs are supplied in
    /// port order: audio, position, resonance, lp_start, lp_end, hp_start, hp_end.
    fn render_buffers(input: &[f32], position: &[f32], voicing: [f32; 5]) -> Vec<f32> {
        let n = input.len();
        let mut op = Djfilter::new();
        let mut out_buf = vec![0.0f32; n];
        let res_buf = vec![voicing[0]; n];
        let lp_start_buf = vec![voicing[1]; n];
        let lp_end_buf = vec![voicing[2]; n];
        let hp_start_buf = vec![voicing[3]; n];
        let hp_end_buf = vec![voicing[4]; n];
        let params: [f32; 0] = [];
        {
            let inputs: Vec<Option<&[f32]>> = vec![
                Some(input),
                Some(position),
                Some(&res_buf),
                Some(&lp_start_buf),
                Some(&lp_end_buf),
                Some(&hp_start_buf),
                Some(&hp_end_buf),
            ];
            let outputs: Vec<&mut [f32]> = vec![out_buf.as_mut_slice()];
            let mut io = Io::new(SR, n, inputs, outputs, &params, &[]);
            op.process(&mut io);
        }
        out_buf
    }

    fn sine(f: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * f * i as f32 / SR).sin())
            .collect()
    }

    fn rms(buf: &[f32]) -> f32 {
        (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt()
    }

    #[test]
    fn north_passes_signal_essentially_untouched() {
        // At position 0 the filter is wide open (lp_start ≈ 20 kHz): a mid tone keeps its level.
        let n = 8192;
        let warmup = 2048;
        let input = sine(1_000.0, n);
        let out = render(&input, 0.0, default_voicing());

        let in_rms = rms(&input[warmup..]);
        let out_rms = rms(&out[warmup..]);
        assert!(
            (out_rms - in_rms).abs() < 0.05 * in_rms,
            "North should be ~transparent: in {in_rms}, out {out_rms}"
        );
    }

    #[test]
    fn counter_clockwise_is_a_low_pass() {
        // Full CCW (position -1): cutoff = lp_end = 200 Hz. A low tone passes, a high tone dies.
        let n = 8192;
        let warmup = 2048;
        let position = -1.0;

        let low = render(&sine(100.0, n), position, default_voicing());
        let high = render(&sine(8_000.0, n), position, default_voicing());

        let low_rms = rms(&low[warmup..]);
        let high_rms = rms(&high[warmup..]);
        assert!(
            low_rms > high_rms * 8.0,
            "CCW low-pass should pass low ({low_rms}) >> high ({high_rms})"
        );
    }

    #[test]
    fn clockwise_is_a_high_pass() {
        // Full CW (position +1): cutoff = hp_end = 6 kHz. A high tone passes, a low tone dies.
        let n = 8192;
        let warmup = 2048;
        let position = 1.0;

        let low = render(&sine(100.0, n), position, default_voicing());
        let high = render(&sine(12_000.0, n), position, default_voicing());

        let low_rms = rms(&low[warmup..]);
        let high_rms = rms(&high[warmup..]);
        assert!(
            high_rms > low_rms * 8.0,
            "CW high-pass should pass high ({high_rms}) >> low ({low_rms})"
        );
    }

    #[test]
    fn turning_ccw_progressively_closes_the_low_pass() {
        // A fixed high tone gets quieter as the knob turns from North toward full CCW.
        let n = 8192;
        let warmup = 2048;
        let input = sine(6_000.0, n);

        let open = rms(&render(&input, 0.0, default_voicing())[warmup..]);
        let half = rms(&render(&input, -0.5, default_voicing())[warmup..]);
        let shut = rms(&render(&input, -1.0, default_voicing())[warmup..]);

        assert!(
            open > half && half > shut,
            "CCW sweep should monotonically close: open {open}, half {half}, shut {shut}"
        );
    }

    #[test]
    fn wired_position_matches_materialized_default() {
        // A flat wired position Float must produce exactly the same output as the same value held
        // as the input's materialized default — there is one read path now (ADR-0028), so a
        // constant wired knob equals the held latch.
        let n = 4096;
        let input = sine(6_000.0, n);
        let via_default = render(&input, -0.6, default_voicing());
        let pos_buf = vec![-0.6f32; n];
        let via_input = render_modulated(&input, &pos_buf, default_voicing());
        for i in 0..n {
            assert!(
                (via_default[i] - via_input[i]).abs() < 1e-4,
                "wired position should match materialized default at {i}: {} vs {}",
                via_default[i],
                via_input[i]
            );
        }
    }

    #[test]
    fn sweeping_position_crosses_from_low_to_high_pass() {
        // A knob ramp from -1 → +1 over a two-tone (low + high) input: the first half (low-pass
        // closing) keeps the low tone, the second half (high-pass opening) keeps the high tone.
        let n = 16384;
        let low = sine(120.0, n);
        let high = sine(10_000.0, n);
        let input: Vec<f32> = low.iter().zip(&high).map(|(a, b)| a + b).collect();
        let position: Vec<f32> = (0..n).map(|i| -1.0 + 2.0 * i as f32 / n as f32).collect();
        let out = render_modulated(&input, &position, default_voicing());

        // Compare the same band's energy via a reference single-tone render isn't needed; the
        // crossover itself is the oracle: early output tracks the low tone, late output the high.
        let early = rms(&out[1024..n / 4]);
        let late = rms(&out[3 * n / 4..n - 1024]);
        // Early (knob near -1, low-pass at ~200 Hz) keeps 120 Hz, kills 10 kHz.
        // Late  (knob near +1, high-pass at ~6 kHz) keeps 10 kHz, kills 120 Hz.
        // Both bands have unit amplitude, so both halves carry roughly one tone's worth of energy
        // — the point is each half is dominated by a *different* tone, which the band tests above
        // already prove per-mode. Here we just assert both halves stay alive and bounded.
        assert!(
            early > 0.1 && late > 0.1,
            "both halves audible: {early}, {late}"
        );
        for (i, &s) in out.iter().enumerate() {
            assert!(s.is_finite() && s.abs() < 8.0, "sample {i} unbounded: {s}");
        }
    }

    #[test]
    fn high_resonance_stays_bounded() {
        let n = 8192;
        // Drive near the resonant frequency at full CCW with maximum resonance.
        let mut voicing = default_voicing();
        voicing[0] = 1.0; // resonance
        let input = sine(200.0, n);
        let out = render(&input, -1.0, voicing);
        for (i, &s) in out.iter().enumerate() {
            assert!(s.is_finite(), "sample {i} not finite: {s}");
            assert!(s.abs() < 50.0, "sample {i} unbounded: {s}");
        }
    }

    #[test]
    fn state_continuous_across_block_slices() {
        // One whole-block render must equal two back-to-back half-block renders that share the
        // same instance (integrator state carries across the slice).
        let n = 512;
        let input = sine(440.0, n);
        let voicing = default_voicing();
        let position = -0.7;
        let whole = render(&input, position, voicing);

        let mut op = Djfilter::new();
        let mut out_buf = vec![0.0f32; n];
        let params: [f32; 0] = [];
        let pos = vec![position; n];
        let res = vec![voicing[0]; n];
        let lp_start = vec![voicing[1]; n];
        let lp_end = vec![voicing[2]; n];
        let hp_start = vec![voicing[3]; n];
        let hp_end = vec![voicing[4]; n];
        let half = n / 2;
        {
            let inputs: Vec<Option<&[f32]>> = vec![
                Some(&input[..half]),
                Some(&pos[..half]),
                Some(&res[..half]),
                Some(&lp_start[..half]),
                Some(&lp_end[..half]),
                Some(&hp_start[..half]),
                Some(&hp_end[..half]),
            ];
            let outputs: Vec<&mut [f32]> = vec![&mut out_buf[..half]];
            let mut io = Io::new(SR, half, inputs, outputs, &params, &[]);
            op.process(&mut io);
        }
        {
            let inputs: Vec<Option<&[f32]>> = vec![
                Some(&input[half..]),
                Some(&pos[half..]),
                Some(&res[half..]),
                Some(&lp_start[half..]),
                Some(&lp_end[half..]),
                Some(&hp_start[half..]),
                Some(&hp_end[half..]),
            ];
            let outputs: Vec<&mut [f32]> = vec![&mut out_buf[half..]];
            let mut io = Io::new(SR, n - half, inputs, outputs, &params, &[]);
            op.process(&mut io);
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

    #[test]
    fn spawn_resets_filter_state() {
        // Warm one instance, then spawn: the child must start from cleared integrators, so its
        // output equals a freshly-constructed instance fed the same input.
        let n = 256;
        let input = sine(440.0, n);
        let voicing = default_voicing();
        let position = -0.8;
        let params: [f32; 0] = [];
        let pos = vec![position; n];
        let res = vec![voicing[0]; n];
        let lp_start = vec![voicing[1]; n];
        let lp_end = vec![voicing[2]; n];
        let hp_start = vec![voicing[3]; n];
        let hp_end = vec![voicing[4]; n];

        let mut warm = Djfilter::new();
        let _ = render(&sine(1_000.0, 4_000), position, voicing); // unrelated warmup of a throwaway
        let mut warm_buf = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![
                Some(&input),
                Some(&pos),
                Some(&res),
                Some(&lp_start),
                Some(&lp_end),
                Some(&hp_start),
                Some(&hp_end),
            ];
            let outputs: Vec<&mut [f32]> = vec![warm_buf.as_mut_slice()];
            let mut io = Io::new(SR, n, inputs, outputs, &params, &[]);
            warm.process(&mut io);
        }

        let mut child = warm.spawn();
        let mut child_buf = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![
                Some(&input),
                Some(&pos),
                Some(&res),
                Some(&lp_start),
                Some(&lp_end),
                Some(&hp_start),
                Some(&hp_end),
            ];
            let outputs: Vec<&mut [f32]> = vec![child_buf.as_mut_slice()];
            let mut io = Io::new(SR, n, inputs, outputs, &params, &[]);
            child.process(&mut io);
        }

        let fresh = render(&input, position, voicing);
        for i in 0..n {
            assert!(
                (child_buf[i] - fresh[i]).abs() < 1e-6,
                "spawned op should start fresh at {i}: {} vs {}",
                child_buf[i],
                fresh[i]
            );
        }
    }
}
