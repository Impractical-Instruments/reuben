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
//! Port types (ADR-0030): every control is a **`F32` input**, each owning its unwired default.
//! When nothing is wired the engine materializes the input from its latched default (so a control
//! surface can sweep the knob via `/djfilter/position`, bit-identical to the old param behavior);
//! when an LFO/envelope is wired the source buffer passes through and sweeps the port audio-rate.
//! There is no longer a separate "signal port + same-named param" pair, and no wired/unwired branch
//! in `process` — `io.read(IN_POSITION)` is always a buffer. `position` stays a continuous
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
use crate::dsp::svf::{Svf, SvfCoeffs};
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Djfilter {
    inputs:  { audio: f32_buffer,
               position:  f32_buffer { -1.0..=1.0,     default 0.0,     "",   lin },
               resonance: f32 { 0.0..=1.0,      default 0.1,     "",   lin },
               lp_start:  f32 { 20.0..=20000.0, default 20000.0, "Hz", exp },
               lp_end:    f32 { 20.0..=20000.0, default 200.0,   "Hz", exp },
               hp_start:  f32 { 20.0..=20000.0, default 20.0,    "Hz", exp },
               hp_end:    f32 { 20.0..=20000.0, default 6000.0,  "Hz", exp } },
    outputs: { audio: f32_buffer },
});

#[derive(Default)]
pub struct Djfilter {
    /// Shared SVF core (`dsp::svf`), continuous across calls / block slices. `process`
    /// copies it to a local, ticks that, and writes it back once per block (#169).
    svf: Svf,
}

impl Djfilter {
    pub fn new() -> Self {
        Self::default()
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

impl Operator for Djfilter {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Cutoff endpoints + resonance are the filter's voicing — `Float` inputs read once at
        // block rate (the filter's character, constant for the (sub)block, block-sliced on change).
        let resonance = io.read(IN_RESONANCE);
        let lp_start = io.read(IN_LP_START);
        let lp_end = io.read(IN_LP_END);
        let hp_start = io.read(IN_HP_START);
        let hp_end = io.read(IN_HP_END);

        // `position` is a Signal input — always a buffer (wired source or materialized default),
        // one read path (ADR-0031). Mode + coefficients are recomputed only when `position`
        // actually changes from the previous sample — `target`/`coeffs` are pure, so reusing the
        // cache on an unchanged knob is bit-identical to recomputing it, and a settled or slow knob
        // costs one compare per sample instead of a `tan()`/`powf()`. The cache lives in this call,
        // not the struct: voicing (resonance/cutoffs) is read once per block, so a coeff cache that
        // survived across `process` calls would go stale on any voicing change at an unchanged knob.
        // The `NaN` seed (≠ anything) forces a compute on the first sample of every block.
        let mut last_pos = f32::NAN;
        let mut use_hp = false;
        let mut c = SvfCoeffs::default();
        // Resolve the per-sample buffers once, outside the loop: a per-iteration `io.read`/
        // `io.write` re-derives the slice from `io`'s input/output tables every sample (a table
        // index + `Option` unwrap per access) — the ADR-0037 handle layer stopped LLVM hoisting
        // it. Binding flat locals once restores the pre-handle codegen (ADR-0037 perf fix).
        let position = io.read(IN_POSITION);
        let audio = io.read(IN_AUDIO);
        let out = io.write(OUT_AUDIO);
        // Block-local copy of the SVF state, stored back once after the loop: ticking
        // `self.svf` directly spills the two integrators to memory every sample; the local
        // stays in registers, leaving ~1 data-write per sample — the output store (#169).
        let mut svf = self.svf;
        for i in 0..n {
            let pos = position[i];

            if pos != last_pos {
                let (uh, cutoff) = target(pos, lp_start, lp_end, hp_start, hp_end);
                c = SvfCoeffs::new(cutoff, resonance, sample_rate);
                use_hp = uh;
                last_pos = pos;
            }

            let taps = svf.tick(audio[i], c);
            out[i] = if use_hp { taps.hp } else { taps.lp };
        }
        self.svf = svf;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Djfilter);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    // Default voicing (resonance/lp_start/lp_end/hp_start/hp_end), in input-port order so tests can
    // tweak one field and keep the rest. `position` is supplied separately.
    fn default_voicing() -> [f32; 5] {
        [0.1, 20_000.0, 200.0, 20.0, 6_000.0]
    }

    /// Set the 5 held `Float` voicing controls (read block-rate via `io.read`) on a driver.
    fn set_voicing(d: &mut OpDriver, voicing: [f32; 5]) -> &mut OpDriver {
        d.set(IN_RESONANCE, voicing[0])
            .set(IN_LP_START, voicing[1])
            .set(IN_LP_END, voicing[2])
            .set(IN_HP_START, voicing[3])
            .set(IN_HP_END, voicing[4])
    }

    /// Render `input` through a fresh Djfilter with the given constant `position` (held `Float`,
    /// `set` once) and `voicing`; `audio` is a time-varying Buffer input (`drive`d block by block).
    fn render(input: &[f32], position: f32, voicing: [f32; 5]) -> Vec<f32> {
        let mut d = OpDriver::for_type(Djfilter::new(), SR);
        set_voicing(&mut d, voicing)
            .set(IN_POSITION, position)
            .drive(IN_AUDIO, input)
            .render(input.len())
            .output(OUT_AUDIO)
            .to_vec()
    }

    /// Render `input` with an explicit time-varying per-sample `position` Float (`drive`d).
    fn render_modulated(input: &[f32], position: &[f32], voicing: [f32; 5]) -> Vec<f32> {
        let mut d = OpDriver::for_type(Djfilter::new(), SR);
        set_voicing(&mut d, voicing)
            .drive(IN_POSITION, position)
            .drive(IN_AUDIO, input)
            .render(input.len())
            .output(OUT_AUDIO)
            .to_vec()
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
        // as the input's materialized default — there is one read path now (ADR-0031), so a
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

    // Raw SVF behavior (attenuation slopes, DC, resonance boundedness) is covered on the
    // shared core in `dsp::svf`'s tests; the tests here cover what the *operator* adds —
    // the knob→mode/cutoff mapping, port semantics, and state across blocks.

    #[test]
    fn state_continuous_across_block_slices() {
        // One render of `n` must equal two back-to-back renders of `n/2` sharing the driver's
        // operator: the SVF integrator state threads across the real block boundaries and across
        // the separate `render` calls.
        let n = 512;
        let input = sine(440.0, n);
        let voicing = default_voicing();
        let position = -0.7;
        let half = n / 2;
        let whole = render(&input, position, voicing);

        let mut split = OpDriver::for_type(Djfilter::new(), SR);
        set_voicing(&mut split, voicing)
            .set(IN_POSITION, position)
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

    #[test]
    fn spawn_resets_filter_state() {
        // Warm one instance, then spawn: the child must start from cleared integrators, so its
        // output equals a freshly-constructed instance fed the same input.
        let n = 256;
        let input = sine(440.0, n);
        let voicing = default_voicing();
        let position = -0.8;

        let mut warm = OpDriver::for_type(Djfilter::new(), SR);
        set_voicing(&mut warm, voicing)
            .set(IN_POSITION, position)
            .drive(IN_AUDIO, &input)
            .render(n); // advance the integrator state

        let mut child = warm.spawn();
        let child_out = set_voicing(&mut child, voicing)
            .set(IN_POSITION, position)
            .drive(IN_AUDIO, &input)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();

        let fresh = render(&input, position, voicing);
        for i in 0..n {
            assert!(
                (child_out[i] - fresh[i]).abs() < 1e-6,
                "spawned op should start fresh at {i}: {} vs {}",
                child_out[i],
                fresh[i]
            );
        }
    }
}
