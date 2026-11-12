//! Sample player — a trigger sampler, one-shot by default, with an optional loop.
//!
//! The first operator to depend on **external decoded audio**: a [`ResourceStore`] built at
//! load time and bound through [`Operator::bind_resources`], read on the RT path through the
//! pure `(id, channel, frame)` accessor (bank-streaming-safe; see [`crate::resources`]).
//!
//! It slots into the **same seam as the oscillator**: it lives inside a voice sub-patch, reading
//! the voice's `freq`/`gate` Signals. Polyphony and steal-oldest come for free from the Voicer
//! hosting one sub-patch per voice.
//!
//! All inputs are Value ports, each owning its unwired default so `/sample/root 60` needs
//! no upstream node; each is read held (the engine block-slices at changes), with `gate` edge-detected
//! at the change frame.
//!
//! - input 0: `freq` — pitch in Hz; the playback rate is `freq / hz(root)` times the file/engine
//!   sample-rate ratio. A non-positive `freq` (the unwired default 0) → plays at `root` pitch,
//!   preserving the old "freq unconnected → root" semantics.
//! - input 1: `gate` — a **rising edge** fires the sample from `start`; each rising edge retriggers.
//!   With `loop` off the hit plays to the buffer end ignoring release; with `loop` on, see below.
//! - input 2: `root` (MIDI) — the pitch at which the sample plays at its natural rate.
//! - input 3: `gain` (linear) — output scale.
//! - input 4: `start` (normalized 0..1) — playback start offset into the buffer.
//! - input 5: `channel` (`i32`) — `-1` downmixes (averages) all channels; `≥0` picks that channel.
//!   A structural selector (the `-1` sentinel + a channel index), so `i32`, not an `Enum`.
//! - input 6: `loop` (0/1, on at ≥ 0.5) — loop mode; off (the default) is the one-shot above.
//! - input 7: `loop_start`, input 8: `loop_end` (normalized 0..1, defaults 0 and 1) — the loop
//!   region. Independent of `start`, so a hit can play in from before the region. Given in either
//!   order (the lower is the start); a region under two source frames does not loop.
//! - output 0: `audio` (`Buffer`).
//!
//! **Loop mode.** While the gate is held, the playhead wraps from `loop_end` back into the region;
//! a gate fall ends the hit with a [`RELEASE_SECONDS`] linear fade, so a gated loop never runs
//! past its gate (the sustain-loop shape: one `gate` both starts and stops it). Each wrap is an
//! equal-power crossfade of [`XFADE_SECONDS`] of source audio: the last stretch before `loop_end`
//! fades out while the first stretch after `loop_start` fades in, and the playhead resumes past
//! that faded-in stretch. Both sides come from inside the region, so it needs no audio beyond
//! either end, at the price of a loop period of `loop_end − loop_start` less the crossfade. The
//! fade is capped at half the region (old or new) so short loops still wrap.
//!
//! The region is **latched per pass**: a hit takes it at the trigger, and each wrap takes the
//! current `loop_start`/`loop_end` at the moment its crossfade begins. Moving the points mid-pass
//! therefore never jumps the playhead; the pass runs on to the end it started with and the next
//! pass loops the new region. A playhead already at or past `loop_end` (a `start` beyond it, or
//! `loop` switched on late) wraps at once. Switching `loop` off lets an in-flight crossfade finish
//! and the hit then plays out to the buffer end.
//!
//! Pitch (the per-hit rate) is **latched at the trigger frame** and fixed for that hit; live
//! pitch-tracking is a deferred param. The fractional playhead is a per-voice `f64` cursor,
//! persistent across blocks (like the oscillator's phase) and reset by [`Operator::spawn`].

use alloc::boxed::Box;
use alloc::sync::Arc;
use core::f32::consts::FRAC_PI_2;
use num_traits::Float;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::resources::{ResolvedRefs, ResourceStore, SampleId};

// Single-source contract: one declaration -> IN_/OUT_/C_ consts + Descriptor, no drift.
crate::operator_contract!(SamplePlayer {
    type_name: "sample",
    inputs:    { freq:       f32 { 0.0..=20000.0, default 0.0, "Hz", lin },
                 gate:       f32 { 0.0..=1.0,     default 0.0, "",   lin },
                 root:       f32 { 0.0..=127.0, default 60.0, "MIDI", lin },
                 gain:       f32 { 0.0..=4.0,   default 1.0,  "",     lin },
                 start:      f32 { 0.0..=1.0,   default 0.0,  "",     lin },
                 channel:    i32 { -1..=31, default -1 },
                 loop:       f32 { 0.0..=1.0,   default 0.0,  "",     lin },
                 loop_start: f32 { 0.0..=1.0,   default 0.0,  "",     lin },
                 loop_end:   f32 { 0.0..=1.0,   default 1.0,  "",     lin } },
    outputs:   { audio: f32_buffer },
    resources: { sample },
});

/// Length of the wrap crossfade, in seconds of *source* audio. Ten milliseconds is past the
/// point where a waveform step reads as a click and short enough that the shortened period is
/// inaudible on an ambient loop.
pub const XFADE_SECONDS: f32 = 0.01;

/// Length of the gate-fall fade that ends a looping hit, in seconds of *engine* time.
pub const RELEASE_SECONDS: f32 = 0.01;

/// A region shorter than this many source frames does not loop: it has no room for a crossfade.
const MIN_LOOP_FRAMES: f64 = 2.0;

/// An in-flight wrap: the fade window `[start, start + len)` ends at the current pass's
/// `loop_end`, and `lo..hi` is the region the next pass loops.
#[derive(Clone, Copy)]
struct Crossfade {
    start: f64,
    len: f64,
    lo: f64,
    hi: f64,
}

#[derive(Default)]
pub struct SamplePlayer {
    /// Shared decoded-audio store (cloned `Arc`), bound at load. `None` until bound.
    store: Option<Arc<ResourceStore>>,
    /// Resolved handle into `store`. `None` until bound.
    sample: Option<SampleId>,
    /// Fractional playhead, in source frames. Per-voice; persists across blocks.
    playhead: f64,
    /// Frames advanced per output sample, latched at the trigger (pitch × SR fold).
    rate: f64,
    /// Whether a hit is currently sounding.
    playing: bool,
    /// Last gate sample seen, for edge detection across block boundaries.
    prev_gate: f32,
    /// The loop region this pass runs to, in source frames, latched at the trigger and at each wrap.
    loop_lo: f64,
    loop_hi: f64,
    /// The wrap in progress, if the playhead is inside a crossfade window.
    xfade: Option<Crossfade>,
    /// Engine frames left in the gate-fall fade; 0 when not releasing.
    release_left: u32,
    /// The gate-fall fade's full length in engine frames, the divisor for `release_left`.
    release_len: u32,
}

impl SamplePlayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// MIDI note → Hz (12-TET, A4 = 440), matching the Voicer's `freq` output convention.
    fn midi_hz(midi: f32) -> f32 {
        440.0 * Float::powf(2.0_f32, (midi - 69.0) / 12.0)
    }
}

/// The crossfade length for a wrap from the region `lo..hi` into `next_lo..next_hi`: `max`, capped
/// at half of either region. `None` when either region is too short to loop.
fn xfade_len(lo: f64, hi: f64, next_lo: f64, next_hi: f64, max: f64) -> Option<f64> {
    let (this_len, next_len) = (hi - lo, next_hi - next_lo);
    (this_len >= MIN_LOOP_FRAMES && next_len >= MIN_LOOP_FRAMES)
        .then(|| max.min(this_len * 0.5).min(next_len * 0.5))
}

/// The loop region in source frames from the normalized inputs, ordered low to high.
fn region(a: f32, b: f32, frames: usize) -> (f64, f64) {
    let a = a.clamp(0.0, 1.0) as f64 * frames as f64;
    let b = b.clamp(0.0, 1.0) as f64 * frames as f64;
    if a <= b { (a, b) } else { (b, a) }
}

impl Operator for SamplePlayer {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let engine_sr = io.sample_rate();
        // Block-rate controls: read once at the top (`Float` inputs, read held via `io.read`).
        let root_hz = Self::midi_hz(io.read(IN_ROOT));
        let gain = io.read(IN_GAIN);
        let start_norm = io.read(IN_START).clamp(0.0, 1.0);
        let channel = io.read(IN_CHANNEL);
        let loop_on = io.read(IN_LOOP) >= 0.5;
        let loop_start = io.read(IN_LOOP_START);
        let loop_end = io.read(IN_LOOP_END);

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
        let (next_lo, next_hi) = region(loop_start, loop_end, frames);
        let xfade_max = (XFADE_SECONDS * store.sample_rate(id)) as f64;

        let mut playhead = self.playhead;
        let mut rate = self.rate;
        let mut playing = self.playing;
        let mut lo = self.loop_lo;
        let mut hi = self.loop_hi;
        let mut xfade = self.xfade;
        let mut release_left = self.release_left;

        // `gate`/`freq` are held Values: the engine block-slices at every change, so this
        // call sees one constant gate level. Detect the rising edge once at frame 0 (the slice's
        // frame 0 *is* the change frame, so the retrigger stays sample-accurate); `prev_gate` carries
        // the level across slices/blocks. Reading the held values here ends the immutable borrow
        // before the per-sample output writes below (keeps `process` alloc-free).
        let g = io.read(IN_GATE);
        if self.prev_gate <= 0.0 && g > 0.0 {
            // A non-positive `freq` (the unwired Value reads its default 0) → play at `root`,
            // preserving the old "freq unconnected → root pitch" semantics.
            let freq = io.read(IN_FREQ);
            let f = if freq > 0.0 { freq } else { root_hz };
            rate = (f as f64 / root_hz as f64) * sr_fold;
            playhead = start_norm as f64 * frames as f64;
            playing = true;
            (lo, hi) = (next_lo, next_hi);
            xfade = None;
            release_left = 0;
        } else if self.prev_gate > 0.0 && g <= 0.0 && loop_on && playing {
            let len = Float::round(RELEASE_SECONDS * engine_sr).max(1.0) as u32;
            self.release_len = len;
            release_left = len;
        }
        self.prev_gate = g;
        // Only a held gate starts a wrap; one already under way finishes either way.
        let wrapping = loop_on && g > 0.0;
        let release_len = self.release_len.max(1) as f32;

        // The frame loop alternates two paths. A *plain run* is the one-shot body and nothing
        // else, run for as many frames as no seam can touch: the whole slice with `loop` off, or up
        // to where the next crossfade would begin. Everything else — entering a crossfade, the
        // fade itself, the wrap, the gate-fall release — takes the *seam path* one frame at a time.
        // Keeping the loop machinery out of the plain run keeps one-shot playback as cheap as it
        // was before loop mode existed.
        let out = io.write(OUT_AUDIO);
        let mut i = 0;
        while i < n {
            if xfade.is_none() && release_left == 0 {
                let mut end = n;
                if wrapping && playing {
                    if let Some(len) = xfade_len(lo, hi, next_lo, next_hi, xfade_max) {
                        let ahead = hi - len - playhead;
                        end = if ahead <= 0.0 {
                            i
                        } else if rate > 0.0 {
                            // Frames whose playhead is still short of the fade window. A rounding
                            // miss only hands a plain frame to the seam path, which plays it the same.
                            let k = Float::ceil(ahead / rate);
                            if k < (n - i) as f64 { i + k as usize } else { n }
                        } else {
                            n
                        };
                    }
                }
                if end > i {
                    for out_sample in out[i..end].iter_mut() {
                        *out_sample = if playing {
                            let base = Float::floor(playhead);
                            let idx = base as usize;
                            if base < 0.0 || idx >= frames {
                                playing = false;
                                0.0
                            } else {
                                let frac = (playhead - base) as f32;
                                let v = interp(&store, id, channel, chans, frames, idx, frac);
                                playhead += rate;
                                v * gain
                            }
                        } else {
                            0.0
                        };
                    }
                    i = end;
                    continue;
                }
            }

            // Seam path: one frame.
            if !playing {
                out[i] = 0.0;
                i += 1;
                continue;
            }
            if wrapping && xfade.is_none() {
                if let Some(len) = xfade_len(lo, hi, next_lo, next_hi, xfade_max) {
                    if playhead >= hi - len {
                        xfade = Some(Crossfade {
                            start: hi - len,
                            len,
                            lo: next_lo,
                            hi: next_hi,
                        });
                    }
                }
            }
            if let Some(xf) = xfade {
                if playhead >= hi {
                    // Resume where the fade-in read had reached, so the seam is continuous. A rate
                    // longer than the region folds back into it rather than escaping.
                    playhead = xf.lo + (playhead - xf.start);
                    if playhead >= xf.hi {
                        playhead = xf.lo + (playhead - xf.lo) % (xf.hi - xf.lo);
                    }
                    (lo, hi) = (xf.lo, xf.hi);
                    xfade = None;
                }
            }
            let base = Float::floor(playhead);
            let idx = base as usize;
            let mut s = if base < 0.0 || idx >= frames {
                playing = false;
                0.0
            } else {
                let frac = (playhead - base) as f32;
                let v = interp(&store, id, channel, chans, frames, idx, frac);
                let v = match xfade {
                    Some(xf) => {
                        let t = if xf.len > 0.0 {
                            ((playhead - xf.start) / xf.len).clamp(0.0, 1.0) as f32
                        } else {
                            1.0
                        };
                        let head = xf.lo + (playhead - xf.start);
                        let hb = Float::floor(head);
                        let h = interp(
                            &store,
                            id,
                            channel,
                            chans,
                            frames,
                            hb as usize,
                            (head - hb) as f32,
                        );
                        let (fade_in, fade_out) = Float::sin_cos(t * FRAC_PI_2);
                        v * fade_out + h * fade_in
                    }
                    None => v,
                };
                playhead += rate;
                v * gain
            };
            if release_left > 0 {
                s *= release_left as f32 / release_len;
                release_left -= 1;
                if release_left == 0 {
                    playing = false;
                    xfade = None;
                }
            }
            out[i] = s;
            i += 1;
        }

        self.playhead = playhead;
        self.rate = rate;
        self.playing = playing;
        self.loop_lo = lo;
        self.loop_hi = hi;
        self.xfade = xfade;
        self.release_left = release_left;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        // Carry the shared resource binding forward; reset per-voice playback state so each
        // voice triggers independently.
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

/// Write `n` frames of silence to the audio output.
fn silence(io: &mut Io, n: usize) {
    for s in io.write(OUT_AUDIO)[..n].iter_mut() {
        *s = 0.0;
    }
}

/// Linearly-interpolated sample at fractional position `idx + frac`, with channel select:
/// `channel < 0` averages all channels (downmix), `≥0` picks one (clamped). Pure — reads go
/// through the store's bounds-checked accessor, so out-of-range frames read as silence.
///
/// Forced inline: with three call sites (the plain run, and the seam path's tail and head reads)
/// the inliner leaves it out of line, and the per-frame call alone costs one-shot playback about a
/// fifth more instructions on the `sample` micro bench.
#[inline(always)]
fn interp(
    store: &ResourceStore,
    id: SampleId,
    channel: i32,
    chans: usize,
    frames: usize,
    idx: usize,
    frac: f32,
) -> f32 {
    let at = |fr: usize| -> f32 {
        if fr >= frames {
            return 0.0;
        }
        if channel < 0 {
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

    const SR: f32 = 48_000.0;

    /// A driver for a player bound (through the real loader path) to a one-resource store holding
    /// `buf` — exercises [`OpDriver::bind`].
    fn bound(buf: crate::resources::SampleBuffer) -> OpDriver {
        let mut d = OpDriver::for_type(SamplePlayer::new(), SR);
        d.bind("sample", buf);
        d
    }

    /// Drive `n` frames; `params` = [root, gain, start, channel] — held controls (`set`). `gate` and
    /// `freq` are held Values fed as edges/changes via `push_gate`/`push_freq` (`None` freq leaves it
    /// at the unwired default 0 → play at root).
    fn run(
        d: &mut OpDriver,
        n: usize,
        gate: &[f32],
        freq: Option<&[f32]>,
        params: [f32; 4],
    ) -> Vec<f32> {
        d.set(IN_ROOT, params[0])
            .set(IN_GAIN, params[1])
            .set(IN_START, params[2])
            .set(IN_CHANNEL, params[3]);
        push_gate(d, gate);
        if let Some(f) = freq {
            push_freq(d, f);
        }
        d.render(n).output(OUT_AUDIO).to_vec()
    }

    /// Drive the now-held-Value `gate` from a dense gate buffer: the gate is fed by edges,
    /// not a per-sample buffer. Push the first frame unconditionally (a continuous render drops the
    /// latch the prior render left set; an unchanged value dedups), then a change at each 0.5
    /// threshold crossing.
    fn push_gate(d: &mut OpDriver, gate: &[f32]) {
        let Some(&first) = gate.first() else { return };
        d.push(IN_GATE, 0, first);
        let mut prev = first;
        for (i, &g) in gate.iter().enumerate().skip(1) {
            if (prev < 0.5) != (g < 0.5) {
                d.push(IN_GATE, i, g);
                prev = g;
            }
        }
    }

    /// Drive the now-held-Value `freq` from a dense buffer: push a held change at each value change
    /// (and at frame 0), so the latch holds the right pitch at the trigger frame.
    fn push_freq(d: &mut OpDriver, freq: &[f32]) {
        let mut prev = f32::NAN;
        for (i, &f) in freq.iter().enumerate() {
            if f != prev {
                d.push(IN_FREQ, i, f);
                prev = f;
            }
        }
    }

    fn mono(samples: &[f32]) -> crate::resources::SampleBuffer {
        crate::resources::SampleBuffer::new(vec![samples.to_vec()], SR)
    }

    // root 69 = A4 = 440 Hz; with file_sr == engine_sr, an unconnected freq → rate 1.0, so
    // the player reproduces the buffer frame-for-frame from the trigger.
    const ROOT_A4: [f32; 4] = [69.0, 1.0, 0.0, -1.0];

    #[test]
    fn rising_edge_triggers_and_plays_to_end() {
        let mut p = bound(mono(&[10.0, 20.0, 30.0, 40.0]));
        let gate = vec![1.0f32; 8]; // rising at frame 0, held
        let out = run(&mut p, 8, &gate, None, ROOT_A4);
        assert_eq!(&out[..4], &[10.0, 20.0, 30.0, 40.0]);
        // One-shot plays to end, then silence (gate still high — release is ignored).
        assert_eq!(&out[4..], &[0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn no_trigger_without_a_rising_edge() {
        let mut p = bound(mono(&[1.0, 2.0, 3.0, 4.0]));
        // Gate already high at frame 0 with prev_gate 0 IS a rising edge, so to test the
        // no-edge case use an all-zero gate.
        let out = run(&mut p, 4, &[0.0, 0.0, 0.0, 0.0], None, ROOT_A4);
        assert_eq!(out, vec![0.0; 4]);
    }

    #[test]
    fn trigger_is_sample_accurate() {
        let mut p = bound(mono(&[10.0, 20.0, 30.0, 40.0]));
        // Gate rises at frame 2.
        let gate = [0.0, 0.0, 1.0, 1.0, 1.0, 1.0];
        let out = run(&mut p, 6, &gate, None, ROOT_A4);
        assert_eq!(out[0], 0.0);
        assert_eq!(out[1], 0.0);
        assert_eq!(&out[2..6], &[10.0, 20.0, 30.0, 40.0]);
    }

    #[test]
    fn gain_scales_output() {
        let mut p = bound(mono(&[10.0, 20.0]));
        let out = run(&mut p, 2, &[1.0, 1.0], None, [69.0, 0.5, 0.0, -1.0]);
        assert_eq!(out, vec![5.0, 10.0]);
    }

    #[test]
    fn start_offset_skips_into_the_buffer() {
        let mut p = bound(mono(&[10.0, 20.0, 30.0, 40.0]));
        // start 0.5 of 4 frames → begin at frame 2.
        let out = run(&mut p, 4, &[1.0; 4], None, [69.0, 1.0, 0.5, -1.0]);
        assert_eq!(&out[..2], &[30.0, 40.0]);
        assert_eq!(&out[2..], &[0.0, 0.0]);
    }

    #[test]
    fn pitch_an_octave_up_doubles_the_rate() {
        let mut p = bound(mono(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]));
        let freq = vec![880.0f32; 8]; // 2× root (A4=440) → rate 2.0
        let out = run(&mut p, 8, &[1.0; 8], Some(&freq), ROOT_A4);
        // Frames 0,2,4,6 then off the end.
        assert_eq!(&out[..4], &[0.0, 2.0, 4.0, 6.0]);
        assert_eq!(&out[4..], &[0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn freq_change_mid_hit_does_not_retune_the_playing_hit() {
        // Pitch is latched at the trigger frame and fixed for the hit (module doc): a `freq`
        // change arriving mid-playback must not warp the in-flight one-shot — a Voicer gliding
        // `freq` into a sounding voice depends on this. `freq` holds 440 (rate 1.0 at ROOT_A4)
        // at the frame-0 trigger, then jumps to 880 at frame 2; the engine block-slices there,
        // but the playing hit must keep rate 1.0 for all 8 frames.
        let mut p = bound(mono(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]));
        let freq = [440.0, 440.0, 880.0, 880.0, 880.0, 880.0, 880.0, 880.0];
        let out = run(&mut p, 8, &[1.0; 8], Some(&freq), ROOT_A4);
        assert_eq!(out, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);

        // Re-gate with the held 880: the next rising edge latches rate 2.0 (frames 0,2,4,6,
        // then off the end) — the latch releases on retrigger, so `freq` is live at edges, not
        // dead. The gate must start low: `prev_gate` carries 1.0 across renders, so frame 0 low
        // + frame 1 high is the rising edge.
        let gate2 = [0.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0];
        let out2 = run(&mut p, 8, &gate2, Some(&[880.0; 8]), ROOT_A4);
        assert_eq!(out2[0], 0.0);
        assert_eq!(&out2[1..5], &[0.0, 2.0, 4.0, 6.0]);
        assert!(out2[5..].iter().all(|&s| s == 0.0), "past the buffer end");
    }

    #[test]
    fn fractional_rate_interpolates_linearly() {
        let mut p = bound(mono(&[0.0, 10.0, 20.0, 30.0, 40.0, 50.0]));
        let freq = vec![660.0f32; 6]; // 1.5× root → rate 1.5
        let out = run(&mut p, 4, &[1.0; 4], Some(&freq), ROOT_A4);
        // playhead 0, 1.5, 3.0, 4.5 → 0, 15, 30, 45.
        assert_eq!(out[0], 0.0);
        assert!((out[1] - 15.0).abs() < 1e-4, "got {}", out[1]);
        assert!((out[2] - 30.0).abs() < 1e-4, "got {}", out[2]);
        assert!((out[3] - 45.0).abs() < 1e-4, "got {}", out[3]);
    }

    #[test]
    fn channel_select_picks_and_downmixes() {
        let left = vec![0.0, 0.0, 0.0, 0.0];
        let right = vec![10.0, 20.0, 30.0, 40.0];
        let buf = crate::resources::SampleBuffer::new(vec![left, right], SR);
        // Pick channel 1 (right).
        let mut p = bound(buf.clone());
        let picked = run(&mut p, 4, &[1.0; 4], None, [69.0, 1.0, 0.0, 1.0]);
        assert_eq!(&picked[..4], &[10.0, 20.0, 30.0, 40.0]);
        // Downmix (channel -1) averages L+R.
        let mut p2 = bound(buf);
        let mixed = run(&mut p2, 4, &[1.0; 4], None, [69.0, 1.0, 0.0, -1.0]);
        assert_eq!(&mixed[..4], &[5.0, 10.0, 15.0, 20.0]);
    }

    #[test]
    fn playhead_is_continuous_across_blocks() {
        // One 8-frame play split over two 4-frame blocks sharing the instance: block 2 must
        // continue where block 1 left off (gate stays high → no retrigger).
        let mut p = bound(mono(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]));
        let a = run(&mut p, 4, &[1.0; 4], None, ROOT_A4);
        let b = run(&mut p, 4, &[1.0; 4], None, ROOT_A4);
        assert_eq!(a, vec![0.0, 1.0, 2.0, 3.0]);
        assert_eq!(b, vec![4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn spawn_carries_binding_but_resets_playback() {
        let mut a = bound(mono(&[10.0, 20.0, 30.0, 40.0]));
        // Advance A partway so its playhead is non-zero.
        let _ = run(&mut a, 2, &[1.0, 1.0], None, ROOT_A4);
        // B shares the store/sample (carried by the op's spawn) but is fresh: a trigger plays from
        // the start.
        let mut b = a.spawn();
        let out = run(&mut b, 4, &[1.0; 4], None, ROOT_A4);
        assert_eq!(out, vec![10.0, 20.0, 30.0, 40.0]);
    }

    #[test]
    fn unbound_player_is_silent() {
        let mut p = OpDriver::for_type(SamplePlayer::new(), SR); // never bound
        let out = run(&mut p, 4, &[1.0; 4], None, ROOT_A4);
        assert_eq!(out, vec![0.0; 4]);
    }

    #[test]
    fn empty_buffer_is_silent() {
        let mut p = bound(crate::resources::SampleBuffer::empty());
        let out = run(&mut p, 4, &[1.0; 4], None, ROOT_A4);
        assert_eq!(out, vec![0.0; 4]);
    }

    #[test]
    fn retrigger_restarts_from_start() {
        let mut p = bound(mono(&[10.0, 20.0, 30.0, 40.0]));
        // Gate: on at 0, off at 1, on again at 2 → second rising edge restarts.
        let gate = [1.0, 0.0, 1.0, 1.0];
        let out = run(&mut p, 4, &gate, None, ROOT_A4);
        // Frame 0: trigger → sample 0 (10). Frame 1: gate low, playhead at 1 → 20.
        // Frame 2: rising edge → restart to frame 0 (10). Frame 3: → 20.
        assert_eq!(out, vec![10.0, 20.0, 10.0, 20.0]);
    }

    // ---- loop mode ----

    /// One second of buffer at `SR` is far longer than the crossfade, so tests can read exact values
    /// outside the seam.
    const LOOP_LEN: usize = 4_800;
    /// The crossfade in source frames for a buffer at `SR` whose loop is longer than twice it.
    const XF: usize = (XFADE_SECONDS * SR) as usize;
    /// The gate-fall fade in engine frames at `SR`.
    const REL: usize = (RELEASE_SECONDS * SR) as usize;

    /// `v[i] = i`: an output sample outside a crossfade reads back as the playhead.
    fn index_ramp(len: usize) -> crate::resources::SampleBuffer {
        mono(&(0..len).map(|i| i as f32).collect::<Vec<_>>())
    }

    fn set_loop(d: &mut OpDriver, on: f32, lo: f32, hi: f32) {
        d.set(IN_LOOP, on).set(IN_LOOP_START, lo).set(IN_LOOP_END, hi);
    }

    fn max_step(out: &[f32]) -> f32 {
        out.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
    }

    #[test]
    fn loop_off_ignores_the_loop_points_and_the_gate_fall() {
        // `loop` 0 is the one-shot, whatever the loop points say: plays to the buffer end and
        // ignores the gate falling partway.
        let mut p = bound(mono(&[10.0, 20.0, 30.0, 40.0]));
        set_loop(&mut p, 0.0, 0.25, 0.5);
        let gate = [1.0, 1.0, 0.0, 0.0, 0.0, 0.0];
        let out = run(&mut p, 6, &gate, None, ROOT_A4);
        assert_eq!(out, vec![10.0, 20.0, 30.0, 40.0, 0.0, 0.0]);
    }

    #[test]
    fn loop_plays_continuously_with_a_period_of_the_region_less_the_crossfade() {
        let mut p = bound(index_ramp(LOOP_LEN));
        set_loop(&mut p, 1.0, 0.0, 1.0);
        let n = LOOP_LEN * 3;
        let out = run(&mut p, n, &vec![1.0; n], None, ROOT_A4);
        // First pass is the plain buffer up to the crossfade.
        assert_eq!(&out[..4], &[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(out[LOOP_LEN - XF - 1], (LOOP_LEN - XF - 1) as f32);
        // After the first seam the playhead resumes at loop_start + crossfade, and every later
        // pass repeats it with period (len - XF).
        let period = LOOP_LEN - XF;
        assert_eq!(out[LOOP_LEN], XF as f32);
        for k in 1..=3 {
            let i = LOOP_LEN + k * 100;
            assert_eq!(out[i], out[i + period], "pass repeats at {i}");
        }
        // It never stops while the gate is held.
        assert!(out[n - 100..].iter().all(|&s| s > 0.0), "still looping at the end");
    }

    #[test]
    fn loop_seam_does_not_click_on_a_buffer_whose_ends_dont_match() {
        // A rising ramp -1..1: a naive wrap jumps by 2.0 at every seam.
        let ramp: Vec<f32> = (0..LOOP_LEN)
            .map(|i| -1.0 + 2.0 * i as f32 / LOOP_LEN as f32)
            .collect();
        let mut p = bound(mono(&ramp));
        set_loop(&mut p, 1.0, 0.0, 1.0);
        let n = LOOP_LEN * 4;
        let out = run(&mut p, n, &vec![1.0; n], None, ROOT_A4);
        let step = max_step(&out[1..]);
        // The ramp's own slope is 2/4800; the equal-power fade adds about (pi/2)/XF per sample.
        assert!(step < 0.01, "largest sample-to-sample jump {step}");
    }

    #[test]
    fn loop_start_before_the_region_plays_in_then_loops_the_region() {
        // `start` is independent of the loop: begin at 0, loop [0.5, 1).
        let mut p = bound(index_ramp(LOOP_LEN));
        set_loop(&mut p, 1.0, 0.5, 1.0);
        let n = LOOP_LEN * 3;
        let out = run(&mut p, n, &vec![1.0; n], None, ROOT_A4);
        assert_eq!(out[100], 100.0, "plays in from start");
        let lo = (LOOP_LEN / 2) as f32;
        // Outside the seams every later sample sits inside the region.
        assert!(out[LOOP_LEN..].iter().all(|&s| s >= lo - 1.0), "stays in the loop");
    }

    #[test]
    fn loop_point_changes_take_effect_at_the_next_wrap() {
        let mut p = bound(index_ramp(LOOP_LEN));
        set_loop(&mut p, 1.0, 0.0, 1.0);
        let a = run(&mut p, 1_000, &[1.0; 1_000], None, ROOT_A4);
        assert_eq!(a[999], 999.0);
        // Move the region below the playhead mid-pass: the current pass still runs on to the
        // old loop_end, then the next pass loops the new region.
        set_loop(&mut p, 1.0, 0.25, 0.5);
        let rest = LOOP_LEN * 3;
        let b = run(&mut p, rest, &vec![1.0; rest], None, ROOT_A4);
        let to_old_end = LOOP_LEN - 1_000 - XF;
        assert_eq!(b[to_old_end - 1], (LOOP_LEN - XF - 1) as f32, "old pass completes");
        let (lo, hi) = ((LOOP_LEN / 4) as f32, (LOOP_LEN / 2) as f32);
        let after = &b[LOOP_LEN - 1_000..];
        assert_eq!(after[0], lo + XF as f32, "lands in the new region");
        // An equal-power blend of two in-region samples can rise to sqrt(2) of the larger.
        let ceiling = hi * core::f32::consts::SQRT_2;
        assert!(
            after.iter().all(|&s| s >= lo && s < ceiling),
            "loops the new region"
        );
        assert!(max_step(&b) < 0.01 * LOOP_LEN as f32, "no hard jump at the moved seam");
    }

    #[test]
    fn switching_loop_off_mid_hit_plays_out_to_the_buffer_end() {
        let mut p = bound(index_ramp(LOOP_LEN));
        set_loop(&mut p, 1.0, 0.0, 1.0);
        let n = LOOP_LEN + 1_000;
        let a = run(&mut p, n, &vec![1.0; n], None, ROOT_A4);
        assert_eq!(a[n - 1], (XF + 999) as f32, "looped once");
        set_loop(&mut p, 0.0, 0.0, 1.0);
        let rest = LOOP_LEN;
        let b = run(&mut p, rest, &vec![1.0; rest], None, ROOT_A4);
        let left = LOOP_LEN - (XF + 1_000);
        assert_eq!(b[left - 1], (LOOP_LEN - 1) as f32, "plays to the last frame");
        assert!(b[left..].iter().all(|&s| s == 0.0), "then stops");
    }

    #[test]
    fn reversed_loop_points_loop_the_same_region() {
        let mut fwd = bound(index_ramp(LOOP_LEN));
        set_loop(&mut fwd, 1.0, 0.25, 0.75);
        let mut rev = bound(index_ramp(LOOP_LEN));
        set_loop(&mut rev, 1.0, 0.75, 0.25);
        let n = LOOP_LEN * 2;
        let a = run(&mut fwd, n, &vec![1.0; n], None, ROOT_A4);
        let b = run(&mut rev, n, &vec![1.0; n], None, ROOT_A4);
        assert_eq!(a, b);
    }

    #[test]
    fn loop_holds_while_the_gate_is_held_and_fades_out_on_gate_fall() {
        let mut p = bound(index_ramp(LOOP_LEN));
        set_loop(&mut p, 1.0, 0.0, 1.0);
        // Hold through two seams, then let go.
        let held = LOOP_LEN * 2;
        let n = held + REL * 3;
        let mut gate = vec![1.0; held];
        gate.resize(n, 0.0);
        let out = run(&mut p, n, &gate, None, ROOT_A4);
        let at_fall = out[held - 1];
        assert!(at_fall > 0.0, "still sounding at the fall");
        // A short linear fade, not a cut: the first released sample is still nearly full.
        assert!(out[held] > at_fall * 0.9, "no cut at the fall");
        assert!(out[held + REL / 2] < out[held], "fading");
        assert!(out[held + REL..].iter().all(|&s| s == 0.0), "silent after the fade");
    }

    #[test]
    fn retrigger_during_the_gate_fall_fade_restarts_the_hit() {
        let mut p = bound(index_ramp(LOOP_LEN));
        set_loop(&mut p, 1.0, 0.0, 1.0);
        let _ = run(&mut p, 1_000, &[1.0; 1_000], None, ROOT_A4);
        let mut gate = vec![0.0; 10];
        gate.extend([1.0; 10]);
        let out = run(&mut p, 20, &gate, None, ROOT_A4);
        assert!(out[9] > 0.0, "fading, not cut");
        assert_eq!(&out[10..14], &[0.0, 1.0, 2.0, 3.0], "restarts from start at full level");
    }

    #[test]
    fn loop_survives_a_very_high_rate() {
        // A rate larger than the whole loop: the playhead still folds back into the region.
        let mut p = bound(index_ramp(64));
        set_loop(&mut p, 1.0, 0.0, 1.0);
        let freq = vec![440.0 * 100.0; 512];
        let out = run(&mut p, 512, &[1.0; 512], Some(&freq), ROOT_A4);
        assert!(out.iter().all(|s| s.is_finite()));
        assert!(out[256..].iter().any(|&s| s != 0.0), "still sounding");
    }

    #[test]
    fn spawn_resets_loop_state() {
        let mut a = bound(index_ramp(LOOP_LEN));
        set_loop(&mut a, 1.0, 0.5, 1.0);
        let n = LOOP_LEN * 2;
        let _ = run(&mut a, n, &vec![1.0; n], None, ROOT_A4);
        let mut b = a.spawn();
        set_loop(&mut b, 1.0, 0.5, 1.0);
        let out = run(&mut b, 4, &[1.0; 4], None, ROOT_A4);
        assert_eq!(out, vec![0.0, 1.0, 2.0, 3.0]);
    }
}
