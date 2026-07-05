//! Filter — state-variable filter; lowpass / highpass / bandpass (V1.3 `mode`, ADR-0022).
//!
//! Port types (ADR-0030): `cutoff` and `resonance` are **`F32` inputs**, each owning its
//! unwired default. When nothing is wired the engine materializes the input from its latched
//! default (so `/filter/cutoff 3000` needs no upstream node, bit-identical to the old param
//! behavior); when an LFO or envelope is wired the source buffer passes through and sweeps the
//! port audio-rate. There is no longer a separate "signal port + same-named param" pair, and no
//! wired/unwired branch in `process` — `io.read(IN_CUTOFF)` is always a buffer.
//!
//! `mode` is an **`Enum` input** [`FilterMode`] {`Lp`, `Hp`, `Bp`}: a held, live-switchable choice
//! read via `io.read(IN_MODE)`. The TPT / Cytomic SVF computes all three responses from the
//! same integrator state, so `mode` selects the output tap (ADR-0022): `lp = v2`, `bp = v1`,
//! `hp = x - k·bp - lp`. `Lp` is the default and bit-identical to the prior lowpass-only filter.
//!
//! - input 0: `audio` (`Buffer`) — the signal to filter.
//! - input 1: `cutoff` (`Float`) — per-sample cutoff in Hz (materialized default 1 kHz).
//! - input 2: `resonance` (`Float`) — per-sample resonance 0..1 (materialized default 0.2).
//! - input 3: `mode` (`Enum` [`FilterMode`] {Lp, Hp, Bp}) — output tap; default `Lp`.
//! - output 0: `audio` (`Buffer`) — the selected response (lowpass / highpass / bandpass).

use crate::descriptor::Descriptor;
use crate::dsp::svf::{Svf, SvfCoeffs, SvfTaps};
use crate::operator::{Io, Operator};
use crate::vocab::FilterMode;

// Single-source contract (ADR-0025/0030): one declaration -> IN_/OUT_ consts and the Descriptor;
// `mode` references the shared `FilterMode` vocab enum (no per-op type), so no drift.
crate::operator_contract!(Filter {
    inputs:  { audio: f32_buffer,
               // signal control with a scalar default (ADR-0031 decision (a)): knob-set/unwired it
               // materializes from 1 kHz, yet an LFO/envelope Signal wires straight in.
               cutoff:    f32_buffer { 20.0..=20_000.0, default 1_000.0, "Hz", exp },
               resonance: f32_buffer { 0.0..=1.0,       default 0.2,     "",   lin },
               mode:      enum(FilterMode) },
    outputs: { audio: f32_buffer },
});

#[derive(Default)]
pub struct Filter {
    /// Shared SVF core (`dsp::svf`), continuous across calls / block slices. `process`
    /// copies it to a local, ticks that, and writes it back once per block (#169).
    svf: Svf,
}

impl Filter {
    pub fn new() -> Self {
        Self::default()
    }
}

/// The held-controls fast path: constant coefficients, one tick per sample. Generic over
/// the tap selector so each [`FilterMode`] gets its own monomorphized loop — the tap
/// choice stays *outside* the sample loop and the unpicked taps' math dead-codes away.
/// With `|t| t.lp` this compiles to exactly the pre-V1.3 lowpass-only loop
/// (bit-identical; a per-sample `match mode` in the loop was measurably not unswitched
/// by LLVM and kept the dead `hp` arithmetic alive).
#[inline]
fn const_block(
    svf: &mut Svf,
    c: SvfCoeffs,
    audio: &[f32],
    out: &mut [f32],
    tap: impl Fn(SvfTaps) -> f32,
) {
    for i in 0..out.len() {
        out[i] = tap(svf.tick(audio[i], c));
    }
}

/// The modulated path: per-sample `cutoff`/`resonance`, coefficients recomputed only when
/// the pair actually changes from the previous sample — a settled or slowly-moving control
/// costs one compare per sample instead of a `tan()`. [`SvfCoeffs::new`] is pure, so
/// reusing the cache on an unchanged input is bit-identical to recomputing every sample. A
/// genuinely audio-rate sweep still recomputes per sample; a coarser control-rate recompute
/// is tracked in #24. Generic over the tap selector for the same per-mode monomorphization
/// as [`const_block`]. The `NaN` seeds force a compute on the first sample (NaN ≠ anything).
#[inline]
#[allow(clippy::too_many_arguments)]
fn modulated_block(
    svf: &mut Svf,
    sample_rate: f32,
    audio: &[f32],
    cutoff_buf: &[f32],
    resonance_buf: &[f32],
    out: &mut [f32],
    tap: impl Fn(SvfTaps) -> f32,
) {
    let mut last_cutoff = f32::NAN;
    let mut last_resonance = f32::NAN;
    let mut c = SvfCoeffs::default();
    for i in 0..out.len() {
        let cutoff = cutoff_buf[i];
        let resonance = resonance_buf[i];
        if cutoff != last_cutoff || resonance != last_resonance {
            c = SvfCoeffs::new(cutoff, resonance, sample_rate);
            last_cutoff = cutoff;
            last_resonance = resonance;
        }
        out[i] = tap(svf.tick(audio[i], c));
    }
}

impl Operator for Filter {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let sample_rate = io.sample_rate();
        let mode = io.read(IN_MODE);

        // Copy the SVF state to a local for the whole block and store it back once at the
        // end. Ticking `self.svf` directly would write the two integrators to memory every
        // sample (LLVM won't promote fields behind `&mut self` across the loop); the local
        // is register-promoted, dropping `process` to ~1 data-write per sample — just the
        // output store (#169).
        let mut svf = self.svf;

        // `cutoff`/`resonance` are Signal inputs — always a buffer (wired source or materialized
        // default), one read path (ADR-0031). When neither changed this block (`varying` false,
        // both held), compute coefficients once — the old fast path, and the `lp` tap is
        // bit-identical to the prior param-only filter.
        if !io.varying(IN_CUTOFF) && !io.varying(IN_RESONANCE) {
            // Held-unchanged this block, so the materialized buffer is uniform — sample 0 is the
            // held value (the former separate held-scalar read of the same latch).
            let cutoff = io.read(IN_CUTOFF)[0];
            let resonance = io.read(IN_RESONANCE)[0];
            let c = SvfCoeffs::new(cutoff, resonance, sample_rate);
            // Resolve the audio slice and output buffer once, outside the loop. A per-sample
            // `io.read(IN_AUDIO)[i]` / `io.write(OUT_AUDIO)[i]` re-derives the slice from `io`'s
            // input/output tables every iteration (a table index + `Option` unwrap per access);
            // the ADR-0037 handle layer stopped LLVM from hoisting that out. Binding two flat
            // locals once restores the pre-handle codegen (ADR-0037 perf fix).
            let audio = io.read(IN_AUDIO);
            let out = io.write(OUT_AUDIO);
            match mode {
                FilterMode::Lp => const_block(&mut svf, c, audio, out, |t| t.lp),
                FilterMode::Bp => const_block(&mut svf, c, audio, out, |t| t.bp),
                FilterMode::Hp => const_block(&mut svf, c, audio, out, |t| t.hp),
            }
            self.svf = svf;
            return;
        }

        // Modulated path: at least one control is dense/changing — read each per sample
        // (audio-rate sweep). Hoist the per-sample buffers to flat locals (see the fast-path
        // note): resolves each slice once instead of re-deriving it from `io` on every
        // `io.read`/`io.write` access.
        let audio = io.read(IN_AUDIO);
        let cutoff_buf = io.read(IN_CUTOFF);
        let resonance_buf = io.read(IN_RESONANCE);
        let out = io.write(OUT_AUDIO);
        match mode {
            FilterMode::Lp => modulated_block(
                &mut svf,
                sample_rate,
                audio,
                cutoff_buf,
                resonance_buf,
                out,
                |t| t.lp,
            ),
            FilterMode::Bp => modulated_block(
                &mut svf,
                sample_rate,
                audio,
                cutoff_buf,
                resonance_buf,
                out,
                |t| t.bp,
            ),
            FilterMode::Hp => modulated_block(
                &mut svf,
                sample_rate,
                audio,
                cutoff_buf,
                resonance_buf,
                out,
                |t| t.hp,
            ),
        }
        self.svf = svf;
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

    // Frequency-response and stability behavior of the SVF itself (attenuation slopes, DC,
    // resonance boundedness) is covered on the shared core in `dsp::svf`'s tests; the tests
    // here cover what the *operator* adds: port semantics, the two coefficient paths, mode
    // selection, and state across blocks.

    #[test]
    fn constant_cutoff_buffer_matches_held_default() {
        // A constant cutoff buffer must produce exactly the same output as the same value held as
        // the input's materialized default — there is one read path now (ADR-0031), so a flat
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

        // Reference: a fresh raw SVF stepping the same once-computed coeffs every sample.
        let mut reference = Svf::default();
        let c = SvfCoeffs::new(2_500.0, 0.0, sr);
        let mut ref_out = vec![0.0f32; n];
        for i in 0..n {
            ref_out[i] = reference.tick(input[i], c).lp;
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
    fn swept_cutoff_is_bit_identical_to_per_sample_recompute() {
        // The other half of the cache contract (see above): under genuinely *changing* controls
        // the recompute-on-change path must still be bit-for-bit identical to recomputing
        // `SvfCoeffs` every sample. Cutoff moves as a staircase (holds each value for 3 samples,
        // so that key repeats) while resonance ramps per sample (that key changes every sample,
        // forcing the per-sample-sweep recompute) — the two keys change on different schedules,
        // so the compare must get both right. (The cache-*hit* branch is bit-checked by the
        // constant-control test above.) `SvfCoeffs::new` is pure, so bit equality is the correct,
        // non-flaky assertion; a lag-by-one cache bug (coefficients from the previous sample's
        // controls) diverges immediately.
        let sr = 48_000.0;
        let n = 4096;
        let input = sine(6_000.0, sr, n);
        let cutoff: Vec<f32> = (0..n).map(|i| 300.0 + ((i / 3) as f32) * 8.0).collect();
        let res: Vec<f32> = (0..n).map(|i| i as f32 / n as f32 * 0.8).collect();
        let out = render_buffers(&input, sr, &cutoff, &res, FilterMode::Lp);

        // Reference: a fresh raw SVF recomputing the coefficients from scratch on every sample.
        let mut reference = Svf::default();
        let mut ref_out = vec![0.0f32; n];
        for i in 0..n {
            ref_out[i] = reference
                .tick(input[i], SvfCoeffs::new(cutoff[i], res[i], sr))
                .lp;
        }
        for i in 0..n {
            assert_eq!(
                out[i].to_bits(),
                ref_out[i].to_bits(),
                "swept-control output diverged from per-sample recompute at {i}"
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
        // The descriptor default `mode` is `Lp`. A driver that never touches IN_MODE seeds the
        // mode latch from the descriptor default through the real `Plan::instantiate` path, so
        // rendering with the *default* mode must be bit-for-bit identical to an explicit
        // lowpass — proving existing instruments, which never set `mode`, are unchanged. (The
        // descriptor *value* `default=0` is pinned by the golden snapshot; this pins the
        // behavioral latch-seeded render.)
        let sr = 48_000.0;
        let n = 4096;
        let input = sine(1_000.0, sr, n);

        // Reference: explicit lowpass via the fast path.
        let lp = render_mode(&input, sr, 1_200.0, 0.4, FilterMode::Lp);

        // Render with matching cutoff/resonance but `mode` never set — the latch keeps the
        // descriptor default.
        let defaulted = OpDriver::for_type(Filter::new(), sr)
            .set(IN_CUTOFF, 1_200.0)
            .set(IN_RESONANCE, 0.4)
            .drive(IN_AUDIO, &input)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        for i in 0..n {
            assert_eq!(
                defaulted[i].to_bits(),
                lp[i].to_bits(),
                "default mode diverged from explicit lowpass at {i}"
            );
        }
    }

    #[test]
    fn every_mode_is_bit_identical_to_its_raw_svf_tap() {
        // `mode` must select exactly the shared core's tap — Lp→lp, Bp→bp, Hp→hp — with no
        // drift in the operator's plumbing. (That each tap *sounds* right — attenuation
        // slopes, DC behavior, the bandpass peak — is proven on the core in `dsp::svf`.)
        let sr = 48_000.0;
        let n = 4096;
        let input = sine(1_000.0, sr, n);
        let c = SvfCoeffs::new(1_200.0, 0.4, sr);

        for (mode, tap) in [
            (FilterMode::Lp, (|t: SvfTaps| t.lp) as fn(SvfTaps) -> f32),
            (FilterMode::Bp, |t: SvfTaps| t.bp),
            (FilterMode::Hp, |t: SvfTaps| t.hp),
        ] {
            let out = render_mode(&input, sr, 1_200.0, 0.4, mode);
            let mut reference = Svf::default();
            for i in 0..n {
                let expect = tap(reference.tick(input[i], c));
                assert_eq!(
                    out[i].to_bits(),
                    expect.to_bits(),
                    "mode {mode:?} diverged from its raw SVF tap at {i}"
                );
            }
        }
    }
}
