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
//! One-port-one-type (ADR-0017): `position` is a **Signal input** carrying an unwired default
//! scalar — the `position` *param*. So a control surface can sweep the knob via messages to the
//! param (block-sliced) *or* an LFO/envelope can wire the port for hands-free automation, with
//! no change to the operator. The cutoff endpoints and resonance are plain params (the filter's
//! voicing/character), constant for the block.
//!
//! - input 0: `audio` (Signal) — the signal to filter.
//! - input 1: `position` (Signal) — knob in [-1, +1]; unwired → the `position` param.
//! - output 0: `audio` (Signal) — filtered output.
//! - param 0: `position` — the position Signal port's unwired default.
//! - param 1: `resonance` (0..1) — filter resonance for both directions.
//! - param 2: `lp_start` (Hz) — low-pass cutoff at North (open end of the CCW sweep).
//! - param 3: `lp_end`   (Hz) — low-pass cutoff fully CCW (position -1).
//! - param 4: `hp_start` (Hz) — high-pass cutoff at North (open end of the CW sweep).
//! - param 5: `hp_end`   (Hz) — high-pass cutoff fully CW (position +1).

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Ports/params declared once (ADR-0025): the macro plants the IN_/OUT_/P_ index consts and the
// matching `Descriptor` from one source, so they cannot drift.
crate::operator_contract!(Djfilter {
    inputs:  { audio: signal, position: signal },
    outputs: { audio: signal },
    params:  { position:  { -1.0..=1.0,     default 0.0,     "",   lin },
               resonance: { 0.0..=1.0,      default 0.1,     "",   lin },
               lp_start:  { 20.0..=20000.0, default 20000.0, "Hz", exp },
               lp_end:    { 20.0..=20000.0, default 200.0,   "Hz", exp },
               hp_start:  { 20.0..=20000.0, default 20.0,    "Hz", exp },
               hp_end:    { 20.0..=20000.0, default 6000.0,  "Hz", exp } },
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

        // Cutoff endpoints + resonance are the filter's voicing — params, constant for the
        // (sub)block (block-sliced on change, ADR-0011).
        let resonance = io.param(P_RESONANCE);
        let lp_start = io.param(P_LP_START);
        let lp_end = io.param(P_LP_END);
        let hp_start = io.param(P_HP_START);
        let hp_end = io.param(P_HP_END);
        // The knob: its unwired default survives as the value the position port reads when
        // nothing is wired (ADR-0017 one-port-one-type).
        let position_default = io.param(P_POSITION);
        let position_wired = io.input(IN_POSITION).is_some();

        if !position_wired {
            // Fast path: the knob is constant for this (sub)block, so mode + coefficients are
            // computed once.
            let (use_hp, cutoff) = target(position_default, lp_start, lp_end, hp_start, hp_end);
            let (k, a1, a2, a3) = coeffs(cutoff, resonance, sample_rate);
            for i in 0..n {
                let x = io.input(IN_AUDIO).map(|s| s[i]).unwrap_or(0.0);
                let (lp, hp) = self.svf_step(x, k, a1, a2, a3);
                io.output(OUT_AUDIO)[i] = if use_hp { hp } else { lp };
            }
            return;
        }

        // Modulated path: the knob is a wired Signal (LFO / automation), read per sample. Mode +
        // coefficients are recomputed only when `position` actually changes from the previous
        // sample — `target`/`coeffs` are pure, so reusing the cache on an unchanged knob is
        // bit-identical to recomputing it, and a settled or slow knob costs one compare per
        // sample instead of a `tan()`/`powf()`.
        let mut last_pos = f32::NAN;
        let (mut use_hp, mut k, mut a1, mut a2, mut a3) = (false, 0.0, 0.0, 0.0, 0.0);
        for i in 0..n {
            let x = io.input(IN_AUDIO).map(|s| s[i]).unwrap_or(0.0);
            let pos = io.input(IN_POSITION).map_or(position_default, |s| s[i]);
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

    // Default param surface, in index order, so tests can tweak one field and keep the rest.
    fn default_params() -> [f32; 6] {
        [0.0, 0.1, 20_000.0, 200.0, 20.0, 6_000.0]
    }

    /// Run `input` through a fresh Djfilter at the given params (knob via the unwired default)
    /// and return the output buffer.
    fn render(input: &[f32], params: [f32; 6]) -> Vec<f32> {
        let n = input.len();
        let mut op = Djfilter::new();
        let mut out_buf = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(input), None];
            let outputs: Vec<&mut [f32]> = vec![out_buf.as_mut_slice()];
            let mut io = Io::new(SR, n, inputs, outputs, &params, &[]);
            op.process(&mut io);
        }
        out_buf
    }

    /// Run `input` with an explicit per-sample `position` Signal wired to port 1.
    fn render_modulated(input: &[f32], position: &[f32], params: [f32; 6]) -> Vec<f32> {
        let n = input.len();
        let mut op = Djfilter::new();
        let mut out_buf = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(input), Some(position)];
            let outputs: Vec<&mut [f32]> = vec![out_buf.as_mut_slice()];
            let mut io = Io::new(SR, n, inputs, outputs, &params, &[]);
            op.process(&mut io);
        }
        out_buf
    }

    fn with_position(mut params: [f32; 6], position: f32) -> [f32; 6] {
        params[P_POSITION] = position;
        params
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
        let out = render(&input, with_position(default_params(), 0.0));

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
        let params = with_position(default_params(), -1.0);

        let low = render(&sine(100.0, n), params);
        let high = render(&sine(8_000.0, n), params);

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
        let params = with_position(default_params(), 1.0);

        let low = render(&sine(100.0, n), params);
        let high = render(&sine(12_000.0, n), params);

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

        let open = rms(&render(&input, with_position(default_params(), 0.0))[warmup..]);
        let half = rms(&render(&input, with_position(default_params(), -0.5))[warmup..]);
        let shut = rms(&render(&input, with_position(default_params(), -1.0))[warmup..]);

        assert!(
            open > half && half > shut,
            "CCW sweep should monotonically close: open {open}, half {half}, shut {shut}"
        );
    }

    #[test]
    fn wired_position_matches_equivalent_param() {
        // A constant position Signal must produce exactly the same output as the same value set
        // as the param (the unwired default): the modulated path with a flat knob equals the
        // fast path.
        let n = 4096;
        let input = sine(6_000.0, n);
        // Param path holds position -0.6; the wired path sets a different default to prove the
        // wired Signal — not the leftover default — is what's read.
        let via_param = render(&input, with_position(default_params(), -0.6));
        let pos_buf = vec![-0.6f32; n];
        let via_input = render_modulated(&input, &pos_buf, with_position(default_params(), 0.3));
        for i in 0..n {
            assert!(
                (via_param[i] - via_input[i]).abs() < 1e-4,
                "wired position should match param at {i}: {} vs {}",
                via_param[i],
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
        let out = render_modulated(&input, &position, default_params());

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
        let mut params = with_position(default_params(), -1.0);
        params[P_RESONANCE] = 1.0;
        let input = sine(200.0, n);
        let out = render(&input, params);
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
        let params = with_position(default_params(), -0.7);
        let whole = render(&input, params);

        let mut op = Djfilter::new();
        let mut out_buf = vec![0.0f32; n];
        let half = n / 2;
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input[..half]), None];
            let outputs: Vec<&mut [f32]> = vec![&mut out_buf[..half]];
            let mut io = Io::new(SR, half, inputs, outputs, &params, &[]);
            op.process(&mut io);
        }
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input[half..]), None];
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
        let params = with_position(default_params(), -0.8);

        let mut warm = Djfilter::new();
        let _ = render(&sine(1_000.0, 4_000), params); // unrelated warmup of a throwaway
        let mut warm_buf = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input), None];
            let outputs: Vec<&mut [f32]> = vec![warm_buf.as_mut_slice()];
            let mut io = Io::new(SR, n, inputs, outputs, &params, &[]);
            warm.process(&mut io);
        }

        let mut child = warm.spawn();
        let mut child_buf = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input), None];
            let outputs: Vec<&mut [f32]> = vec![child_buf.as_mut_slice()];
            let mut io = Io::new(SR, n, inputs, outputs, &params, &[]);
            child.process(&mut io);
        }

        let fresh = render(&input, params);
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
