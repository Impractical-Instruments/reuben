//! Granulator — a free-running granular cloud over a loaded sound file (ADR-0016).
//!
//! Like the [`sample`](super::sample) player it depends on **external decoded audio**: a
//! [`ResourceStore`] built at load time and bound through [`Operator::bind_resources`], read on the
//! RT path through the pure `(id, channel, frame)` accessor. Unlike the one-shot sampler, it
//! continuously spawns short overlapping **grains** — each a windowed read of the buffer at the
//! current scrub position, pitch-shifted and enveloped — and sums them into one mono stream. The
//! defining granular trick is that **position and pitch are decoupled**: you can freeze/scrub the
//! read position while pitch holds, or transpose while position holds.
//!
//! Grains overlap, so the operator owns a **fixed internal pool** of [`MAX_GRAINS`] grain voices,
//! allocated up front (RT-safe: `process` never allocates). When the cloud is denser than the pool
//! can hold, a would-be spawn is **skipped** (density effectively caps at the pool size). Spray
//! (per-grain start jitter) draws from a seeded inline xorshift32 so renders stay deterministic
//! (ADR-0001); `spawn` resets it to a fixed seed, like [`noise`](super::noise).
//!
//! Signal inputs latch **per grain at its spawn frame** — modulating `position`/`pitch`/`grain_size`
//! with an LFO sweeps the cloud, each new grain capturing the value live at its birth.
//!
//! - input 0: `position` (signal, 0..1) — normalized scrub point into the buffer.
//! - input 1: `grain_size` (signal, ms) — each grain's length.
//! - input 2: `pitch` (signal, semitones) — grain transpose; rate = `2^(pitch/12)` × file/engine SR fold.
//! - input 3: `density` (Hz) — grains spawned per second (spawn interval = `sample_rate / density`).
//! - input 4: `spray` (ms) — max ± random jitter applied to each grain's start position.
//! - input 5: `gain` (linear) — output scale.
//! - input 6: `channel` — `-1` downmixes (averages) all channels; `≥0` picks that channel.
//! - input 7: `window` ([`GrainWindow`]) — the grain amplitude envelope (Hann/Triangle/Tukey/Rect).
//! - output 0: `audio` (`Buffer`) — the summed grain cloud.

use std::sync::Arc;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::resources::{ResolvedRefs, ResourceStore, SampleId};
use crate::vocab::GrainWindow;

// Single-source contract (ADR-0025/0030): one declaration -> IN_/OUT_/P_ consts + Descriptor, no drift.
// `position`/`grain_size`/`pitch` are signal controls with scalar defaults (ADR-0031 decision (a),
// like the oscillator's `freq`): knob-set or unwired they materialize from the default, yet an LFO
// Signal wires straight in and is latched per grain. `density`/`spray`/`gain`/`channel` are held
// Values; `window` references the shared `GrainWindow` vocab enum.
crate::operator_contract!(Granulator {
    type_name: "granulator",
    inputs: { position: f32_buffer { 0.0..=1.0, default 0.0, "", lin },
              grain_size: f32_buffer { 5.0..=500.0, default 100.0, "ms", lin },
              pitch: f32_buffer { -24.0..=24.0, default 0.0, "st", lin },
              density: f32 { 1.0..=200.0, default 20.0, "Hz", exp },
              spray: f32 { 0.0..=1000.0, default 0.0, "ms", lin },
              gain: f32 { 0.0..=4.0, default 1.0, "", lin },
              channel: f32 { -1.0..=31.0, default -1.0, "", lin },
              window: enum(GrainWindow) },
    outputs: { audio: f32_buffer },
    resources: { sample },
});

/// Maximum concurrent grains the internal pool holds. A would-be spawn with no free slot is skipped,
/// so density caps here rather than allocating on the hot path. 32 covers generous density×size
/// overlap (e.g. 50 grains/s × 500 ms grains ≈ 25 concurrent).
const MAX_GRAINS: usize = 32;

/// Fixed deterministic seed the spray PRNG starts from. Non-zero (xorshift can't leave the zero
/// state); an arbitrary odd constant — the value only matters for reproducibility.
const SEED: u32 = 0x9E37_79B9;

/// One grain voice in the pool. `pos`/`len` are in output samples; `playhead` is a fractional
/// source-frame cursor (like the sampler's), advanced by `rate` per output sample.
#[derive(Clone, Copy, Default)]
struct Grain {
    /// Whether this slot is currently sounding.
    active: bool,
    /// Fractional read position in source frames, advanced by `rate` each output sample.
    playhead: f64,
    /// Source frames advanced per output sample (pitch × SR fold), latched at spawn.
    rate: f64,
    /// Output samples elapsed since the grain started; the window phase is `pos / len`.
    pos: f64,
    /// Grain length in output samples, latched at spawn.
    len: f64,
}

pub struct Granulator {
    /// Shared decoded-audio store (cloned `Arc`), bound at load. `None` until bound.
    store: Option<Arc<ResourceStore>>,
    /// Resolved handle into `store`. `None` until bound.
    sample: Option<SampleId>,
    /// The fixed grain pool. Persists across blocks; reset by [`Operator::spawn`].
    grains: [Grain; MAX_GRAINS],
    /// Output samples until the next grain spawn; counts down per sample, += interval on spawn.
    /// Starts at 0 so the first grain fires at frame 0. Continuous across blocks.
    spawn_counter: f64,
    /// xorshift32 state for spray. Continuous across blocks; reset to `SEED` on `spawn`.
    rng: u32,
}

impl Default for Granulator {
    fn default() -> Self {
        Self {
            store: None,
            sample: None,
            grains: [Grain::default(); MAX_GRAINS],
            spawn_counter: 0.0,
            rng: SEED,
        }
    }
}

impl Granulator {
    pub fn new() -> Self {
        Self::default()
    }

    /// One xorshift32 step mapped to a uniform float in [-1, 1) — the spray jitter draw. Marsaglia's
    /// (13, 17, 5) triple; full period over the non-zero u32s, so the state never collapses to 0.
    #[inline]
    fn next_unit(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        let bits = x >> 8; // top 24 bits → [0, 2^24)
        (bits as f32) * (1.0 / (1u32 << 24) as f32) * 2.0 - 1.0
    }
}

impl Operator for Granulator {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let engine_sr = io.sample_rate();

        // Block-rate held controls (ADR-0031): read once at the top.
        let density = io
            .input::<f32>(IN_DENSITY)
            .unwrap_or(20.0)
            .clamp(1.0, 200.0);
        let spray_ms = io.input::<f32>(IN_SPRAY).unwrap_or(0.0).max(0.0);
        let gain = io.input::<f32>(IN_GAIN).unwrap_or(1.0);
        let channel = io.input::<f32>(IN_CHANNEL).unwrap_or(-1.0);
        let window = io.input::<GrainWindow>(IN_WINDOW).unwrap_or_default();

        // Resolve the binding; unbound, missing, or empty → silence.
        let store = match &self.store {
            Some(s) => s.clone(),
            None => return silence(io, n),
        };
        let id = match self.sample {
            Some(i) => i,
            None => return silence(io, n),
        };
        let frames = store.frames(id);
        let chans = store.channels(id);
        if frames == 0 || chans == 0 {
            return silence(io, n);
        }
        let sr_fold = if engine_sr > 0.0 {
            store.sample_rate(id) as f64 / engine_sr as f64
        } else {
            0.0
        };

        let spawn_interval = (engine_sr as f64 / density as f64).max(1.0);
        // Spray is a duration; convert to source frames (the playhead's units).
        let spray_frames = (spray_ms as f64 / 1000.0) * store.sample_rate(id) as f64;

        // Signal controls are read per grain at its spawn frame. These slices borrow the arena, not
        // `io`, so they stay valid alongside the mutable `out` borrow below (the oscillator pattern).
        let position = io.input::<&[f32]>(IN_POSITION);
        let grain_size = io.input::<&[f32]>(IN_GRAIN_SIZE);
        let pitch = io.input::<&[f32]>(IN_PITCH);

        let mut spawn_counter = self.spawn_counter;
        let out = io.output::<&mut [f32]>(OUT_AUDIO);

        for (i, slot) in out.iter_mut().enumerate().take(n) {
            // Spawn any grains due at this frame (interval ≥ 1, so at most one in practice).
            while spawn_counter <= 0.0 {
                let pos_norm = position.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
                let size_ms = grain_size
                    .get(i)
                    .copied()
                    .unwrap_or(100.0)
                    .clamp(5.0, 500.0);
                let semis = pitch.get(i).copied().unwrap_or(0.0).clamp(-24.0, 24.0);

                let offset = if spray_frames > 0.0 {
                    self.next_unit() as f64 * spray_frames
                } else {
                    0.0
                };
                let start =
                    (pos_norm as f64 * frames as f64 + offset).clamp(0.0, (frames - 1) as f64);
                let len = (size_ms as f64 / 1000.0 * engine_sr as f64).max(1.0);
                let rate = 2.0_f64.powf(semis as f64 / 12.0) * sr_fold;

                // Skip on overflow: no free slot ⇒ density caps at the pool size (no allocation).
                if let Some(g) = self.grains.iter_mut().find(|g| !g.active) {
                    *g = Grain {
                        active: true,
                        playhead: start,
                        rate,
                        pos: 0.0,
                        len,
                    };
                }
                spawn_counter += spawn_interval;
            }
            spawn_counter -= 1.0;

            // Sum every active grain at this output frame.
            let mut s = 0.0f32;
            for g in self.grains.iter_mut() {
                if !g.active {
                    continue;
                }
                let env = window_env(window, (g.pos / g.len) as f32);
                s += interp(&store, id, channel, chans, frames, g.playhead) * env;
                g.playhead += g.rate;
                g.pos += 1.0;
                if g.pos >= g.len {
                    g.active = false;
                }
            }
            *slot = s * gain;
        }

        self.spawn_counter = spawn_counter;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        // Carry the shared resource binding forward; reset the grain pool, spawn clock, and PRNG seed
        // so each voice grains independently and reproducibly (ADR-0016).
        Box::new(Self {
            store: self.store.clone(),
            sample: self.sample,
            ..Self::default()
        })
    }

    fn bind_resources(&mut self, store: &Arc<ResourceStore>, refs: &ResolvedRefs) {
        self.store = Some(store.clone());
        self.sample = refs.get("sample");
    }
}

crate::register_operator!(Granulator);

/// Write `n` frames of silence to the audio output.
fn silence(io: &mut Io, n: usize) {
    io.output::<&mut [f32]>(OUT_AUDIO)[..n].fill(0.0);
}

/// The grain amplitude envelope at normalized phase `x` ∈ [0, 1). Each shape is 0 at the edges
/// (except `Rect`) and peaks mid-grain, so overlapping grains crossfade without clicks.
#[inline]
fn window_env(window: GrainWindow, x: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let x = x.clamp(0.0, 1.0);
    match window {
        GrainWindow::Hann => 0.5 - 0.5 * (TAU * x).cos(),
        GrainWindow::Triangle => 1.0 - (2.0 * x - 1.0).abs(),
        GrainWindow::Tukey => {
            // Flat-top with cosine tapers over the outer 25% on each side (α = 0.5).
            const ALPHA: f32 = 0.5;
            const HALF: f32 = ALPHA / 2.0;
            if x < HALF {
                0.5 * (1.0 + (PI * (2.0 * x / ALPHA - 1.0)).cos())
            } else if x > 1.0 - HALF {
                0.5 * (1.0 + (PI * (2.0 * x / ALPHA - 2.0 / ALPHA + 1.0)).cos())
            } else {
                1.0
            }
        }
        GrainWindow::Rect => 1.0,
    }
}

/// Linearly-interpolated sample at fractional source position `playhead`, with channel select:
/// `channel < 0` averages all channels (downmix), `≥0` picks one (clamped). Pure — reads go through
/// the store's bounds-checked accessor, so out-of-range frames read as silence.
fn interp(
    store: &ResourceStore,
    id: SampleId,
    channel: f32,
    chans: usize,
    frames: usize,
    playhead: f64,
) -> f32 {
    let base = playhead.floor();
    if base < 0.0 {
        return 0.0;
    }
    let idx = base as usize;
    let frac = (playhead - base) as f32;
    let at = |fr: usize| -> f32 {
        if fr >= frames {
            return 0.0;
        }
        if channel < 0.0 {
            let mut sum = 0.0;
            for ch in 0..chans {
                sum += store.sample(id, ch, fr);
            }
            sum / chans as f32
        } else {
            let ch = (channel as usize).min(chans - 1);
            store.sample(id, ch, fr)
        }
    };
    let a = at(idx);
    let b = at(idx + 1);
    a + (b - a) * frac
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;
    use crate::resources::SampleBuffer;

    const SR: f32 = 48_000.0;

    fn mono(samples: &[f32]) -> SampleBuffer {
        SampleBuffer::new(vec![samples.to_vec()], SR)
    }

    /// A driver bound (through the real loader path) to a one-resource store holding `buf`.
    fn bound(buf: SampleBuffer) -> OpDriver {
        let mut d = OpDriver::for_type(Granulator::new(), SR);
        d.bind("sample", buf);
        d
    }

    /// Drive `n` frames at constant controls. `p` = [position, grain_size, pitch, density, spray,
    /// gain, channel]; all inputs are held/materialized constants here (`set`).
    fn run(d: &mut OpDriver, n: usize, window: GrainWindow, p: [f32; 7]) -> Vec<f32> {
        d.set(IN_POSITION, p[0])
            .set(IN_GRAIN_SIZE, p[1])
            .set(IN_PITCH, p[2])
            .set(IN_DENSITY, p[3])
            .set(IN_SPRAY, p[4])
            .set(IN_GAIN, p[5])
            .set(IN_CHANNEL, p[6])
            .set(IN_WINDOW, window);
        d.render(n).output(OUT_AUDIO).to_vec()
    }

    /// A 240-sample (5 ms @ 48 kHz) ramp: `buf[k] = k`. One Rect grain of 5 ms reads it verbatim.
    fn ramp() -> Vec<f32> {
        (0..240).map(|k| k as f32).collect()
    }

    // 5 ms @ 48 kHz = exactly 240 frames; density 20 ⇒ a 2400-sample spawn interval, so a single
    // grain sounds over the first 240 frames with the next not due until frame 2400.
    const GRAIN_5MS_DENSITY_20: [f32; 7] = [0.0, 5.0, 0.0, 20.0, 0.0, 1.0, -1.0];

    #[test]
    fn rect_grain_reproduces_the_buffer_across_blocks() {
        // Rect window, pitch 0 (rate 1.0 at file_sr == engine_sr), position 0 → the grain copies the
        // buffer frame-for-frame. 240 frames spans two real 128-frame blocks, so this also pins
        // cross-block grain continuity.
        let buf = ramp();
        let mut d = bound(mono(&buf));
        let out = run(&mut d, 240, GrainWindow::Rect, GRAIN_5MS_DENSITY_20);
        for (k, &got) in out.iter().enumerate() {
            assert!((got - k as f32).abs() < 1e-3, "frame {k}: {got} != {k}");
        }
    }

    #[test]
    fn hann_window_shapes_the_grain() {
        // A constant buffer isolates the envelope: out == window(phase). Hann is 0 at the edges and
        // peaks at 1.0 mid-grain (phase 0.5 → frame 120 of 240).
        let buf = vec![1.0f32; 240];
        let mut d = bound(mono(&buf));
        let out = run(&mut d, 240, GrainWindow::Hann, GRAIN_5MS_DENSITY_20);
        assert!(out[0].abs() < 1e-3, "Hann starts near 0, got {}", out[0]);
        assert!(
            (out[120] - 1.0).abs() < 2e-2,
            "Hann peaks ~1 at center, got {}",
            out[120]
        );
        // Symmetric: the rise to center mirrors the fall from it.
        assert!(
            (out[60] - out[180]).abs() < 2e-2,
            "Hann should be symmetric"
        );
    }

    #[test]
    fn rect_window_is_flat() {
        // Rect over a constant buffer is unity across the whole grain (no taper).
        let buf = vec![1.0f32; 240];
        let mut d = bound(mono(&buf));
        let out = run(&mut d, 240, GrainWindow::Rect, GRAIN_5MS_DENSITY_20);
        for (k, &got) in out.iter().enumerate() {
            assert!((got - 1.0).abs() < 1e-4, "frame {k}: rect not flat ({got})");
        }
    }

    #[test]
    fn pitch_up_an_octave_doubles_the_read_rate() {
        // +12 semitones → rate 2.0: the grain reads every other source frame.
        let buf = ramp();
        let mut d = bound(mono(&buf));
        let out = run(
            &mut d,
            8,
            GrainWindow::Rect,
            [0.0, 5.0, 12.0, 20.0, 0.0, 1.0, -1.0],
        );
        for (k, &got) in out.iter().take(4).enumerate() {
            let want = (2 * k) as f32;
            assert!((got - want).abs() < 1e-3, "frame {k}: {got} != {want}");
        }
    }

    #[test]
    fn gain_scales_output() {
        let buf = vec![1.0f32; 240];
        let mut d = bound(mono(&buf));
        let out = run(
            &mut d,
            16,
            GrainWindow::Rect,
            [0.0, 5.0, 0.0, 20.0, 0.0, 0.5, -1.0],
        );
        for (k, &got) in out.iter().enumerate() {
            assert!(
                (got - 0.5).abs() < 1e-4,
                "frame {k}: gain not applied ({got})"
            );
        }
    }

    #[test]
    fn density_sets_the_spawn_interval() {
        // Non-overlapping grains (5 ms grain, 2400-sample interval): buf starts at 1.0 so each grain
        // onset is a 0→nonzero edge. Over 2401 frames, grains fire at frame 0 and frame 2400.
        let buf: Vec<f32> = (0..240).map(|k| (k + 1) as f32).collect();
        let mut d = bound(mono(&buf));
        let out = run(&mut d, 2401, GrainWindow::Rect, GRAIN_5MS_DENSITY_20);
        let mut prev = 0.0f32;
        let onsets = out
            .iter()
            .filter(|&&s| {
                let edge = prev == 0.0 && s != 0.0;
                prev = s;
                edge
            })
            .count();
        assert_eq!(onsets, 2, "density 20 over 2401 frames → 2 grain onsets");
    }

    #[test]
    fn channel_select_picks_and_downmixes() {
        let left = vec![0.0f32; 240];
        let right = ramp();
        let buf = SampleBuffer::new(vec![left, right], SR);
        // Pick channel 1 (right): out == right.
        let mut d = bound(buf.clone());
        let picked = run(
            &mut d,
            240,
            GrainWindow::Rect,
            [0.0, 5.0, 0.0, 20.0, 0.0, 1.0, 1.0],
        );
        assert!(
            (picked[100] - 100.0).abs() < 1e-3,
            "picked ch1: {}",
            picked[100]
        );
        // Downmix (-1) averages L+R = right/2.
        let mut d2 = bound(buf);
        let mixed = run(&mut d2, 240, GrainWindow::Rect, GRAIN_5MS_DENSITY_20);
        assert!((mixed[100] - 50.0).abs() < 1e-3, "downmix: {}", mixed[100]);
    }

    #[test]
    fn spray_is_deterministic_and_audible() {
        // Same seed ⇒ bit-identical renders even with spray on (ADR-0001).
        let buf = ramp();
        let params = [0.3, 10.0, 0.0, 60.0, 300.0, 1.0, -1.0];
        let a = run(&mut bound(mono(&buf)), 4096, GrainWindow::Hann, params);
        let b = run(&mut bound(mono(&buf)), 4096, GrainWindow::Hann, params);
        assert_eq!(a, b, "spray must be reproducible under the seeded PRNG");
        // Spray actually perturbs the cloud: turning it off changes the output.
        let no_spray = run(
            &mut bound(mono(&buf)),
            4096,
            GrainWindow::Hann,
            [0.3, 10.0, 0.0, 60.0, 0.0, 1.0, -1.0],
        );
        assert_ne!(a, no_spray, "spray > 0 should differ from spray 0");
    }

    #[test]
    fn unbound_granulator_is_silent() {
        let mut d = OpDriver::for_type(Granulator::new(), SR); // never bound
        let out = run(&mut d, 256, GrainWindow::Hann, GRAIN_5MS_DENSITY_20);
        assert_eq!(out, vec![0.0; 256]);
    }

    #[test]
    fn empty_buffer_is_silent() {
        let mut d = bound(SampleBuffer::empty());
        let out = run(&mut d, 256, GrainWindow::Hann, GRAIN_5MS_DENSITY_20);
        assert_eq!(out, vec![0.0; 256]);
    }

    #[test]
    fn spawn_carries_binding_but_resets_grains() {
        // Advance A partway so its grain pool / spawn clock are non-fresh.
        let buf = ramp();
        let mut a = bound(mono(&buf));
        let _ = run(&mut a, 100, GrainWindow::Hann, GRAIN_5MS_DENSITY_20);
        // B shares the store/sample (carried by spawn) but starts fresh: a grain fires at frame 0
        // and reproduces the buffer, exactly like a never-advanced instance.
        let mut b = a.spawn();
        let out = run(&mut b, 240, GrainWindow::Rect, GRAIN_5MS_DENSITY_20);
        for (k, &got) in out.iter().enumerate() {
            assert!((got - k as f32).abs() < 1e-3, "frame {k}: {got} != {k}");
        }
    }

    #[test]
    fn dense_long_grains_stay_bounded_no_panic() {
        // Max density × max grain size oversubscribes the pool; overflow-skip must keep the render
        // finite and panic-free (no out-of-bounds on the fixed pool).
        let buf: Vec<f32> = (0..48_000).map(|k| ((k as f32) * 0.001).sin()).collect();
        let mut d = bound(mono(&buf));
        let out = run(
            &mut d,
            48_000,
            GrainWindow::Hann,
            [0.5, 500.0, 0.0, 200.0, 100.0, 1.0, -1.0],
        );
        assert!(out.iter().all(|s| s.is_finite()), "output must stay finite");
        assert!(
            out.iter().any(|&s| s != 0.0),
            "a dense cloud must make sound"
        );
    }
}
