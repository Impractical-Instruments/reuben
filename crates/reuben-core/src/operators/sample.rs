//! Sample player — a one-shot trigger sampler (ADR-0016).
//!
//! The first operator to depend on **external decoded audio**: a [`ResourceStore`] built at
//! load time and bound through [`Operator::bind_resources`], read on the RT path through the
//! pure `(id, channel, frame)` accessor (bank-streaming-safe; see [`crate::resources`]).
//!
//! It slots into the **same seam as the oscillator** (ADR-0010): downstream of a Voicer,
//! reading the Voicer's per-Voice `freq`/`gate` Signals. Polyphony and steal-oldest come for
//! free from the Voicer's Lane fan-out.
//!
//! Port types (ADR-0030): `freq`/`gate` are `Buffer` wire-ins (read per sample via `io.signal`);
//! the former params `root`/`gain`/`start`/`channel` are `Float` inputs, each owning its unwired
//! default so `/sample/root 60` needs no upstream node — read once per block as the held (ZOH)
//! value via `io.last(port)`.
//!
//! - input 0: `freq` (`Buffer`) — pitch in Hz; the playback rate is `freq / hz(root)` times
//!   the file/engine sample-rate ratio. A non-positive `freq` (an unwired buffer reads 0)
//!   → plays at `root` pitch, preserving the old "freq unconnected → root" semantics.
//! - input 1: `gate` (`Buffer`) — a **rising edge** fires the sample from `start`; one-shot
//!   plays to the buffer end ignoring release; each rising edge retriggers.
//! - input 2: `root` (`Float`, MIDI) — the pitch at which the sample plays at its natural rate.
//! - input 3: `gain` (`Float`, linear) — output scale.
//! - input 4: `start` (`Float`, normalized 0..1) — playback start offset into the buffer.
//! - input 5: `channel` (`Float`) — `-1` downmixes (averages) all channels; `≥0` picks that
//!   channel. A continuous/structural selector (rounded in `process`), not an `Enum`.
//! - output 0: `audio` (`Buffer`).
//!
//! Pitch (the per-hit rate) is **latched at the trigger frame** and fixed for that hit; live
//! pitch-tracking is a deferred param. The fractional playhead is a per-Lane `f64` cursor,
//! persistent across blocks (like the oscillator's phase) and reset by [`Operator::spawn`].

use std::sync::Arc;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::resources::{ResolvedRefs, ResourceStore, SampleId};

// Single-source contract (ADR-0025): one declaration -> IN_/OUT_/P_ consts + Descriptor, no drift.
crate::operator_contract!(SamplePlayer {
    type_name: "sample",
    inputs:    { freq:    buffer,
                 gate:    buffer,
                 root:    float { 0.0..=127.0, default 60.0, "MIDI", lin },
                 gain:    float { 0.0..=4.0,   default 1.0,  "",     lin },
                 start:   float { 0.0..=1.0,   default 0.0,  "",     lin },
                 channel: float { -1.0..=31.0, default -1.0, "",     lin } },
    outputs:   { audio: buffer },
    resources: { sample },
});

#[derive(Default)]
pub struct SamplePlayer {
    /// Shared decoded-audio store (cloned `Arc`), bound at load. `None` until bound.
    store: Option<Arc<ResourceStore>>,
    /// Resolved handle into `store`. `None` until bound.
    sample: Option<SampleId>,
    /// Fractional playhead, in source frames. Per-Lane; persists across blocks.
    playhead: f64,
    /// Frames advanced per output sample, latched at the trigger (pitch × SR fold).
    rate: f64,
    /// Whether a one-shot is currently sounding.
    playing: bool,
    /// Last gate sample seen, for rising-edge detection across block boundaries.
    prev_gate: f32,
}

impl SamplePlayer {
    pub fn new() -> Self {
        Self::default()
    }

    /// MIDI note → Hz (12-TET, A4 = 440), matching the Voicer's `freq` output convention.
    fn midi_hz(midi: f32) -> f32 {
        440.0 * 2.0_f32.powf((midi - 69.0) / 12.0)
    }
}

impl Operator for SamplePlayer {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let engine_sr = io.sample_rate();
        // Block-rate controls: read once at the top (ADR-0030 `Float` inputs, held via `io.last`).
        let root_hz = Self::midi_hz(io.last::<f32>(IN_ROOT).unwrap_or(60.0));
        let gain = io.last::<f32>(IN_GAIN).unwrap_or(1.0);
        let start_norm = io.last::<f32>(IN_START).unwrap_or(0.0).clamp(0.0, 1.0);
        let channel = io.last::<f32>(IN_CHANNEL).unwrap_or(-1.0);

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

        let mut prev = self.prev_gate;
        let mut playhead = self.playhead;
        let mut rate = self.rate;
        let mut playing = self.playing;

        for i in 0..n {
            // `gate`/`freq` are `Float` inputs — always a buffer (wired source or the materialized
            // carrier). Read one sample at a time so each immutable borrow of `io` ends before the
            // mutable output write (keeps `process` alloc-free, mirrors the oscillator).
            let g = io.signal(IN_GATE).get(i).copied().unwrap_or(0.0);
            // Rising edge → (re)trigger: latch pitch and reset the playhead to `start`.
            if prev <= 0.0 && g > 0.0 {
                // A non-positive `freq` (the unwired carrier materializes to 0) → play at `root`,
                // preserving the old "freq unconnected → root pitch" semantics.
                let freq = io.signal(IN_FREQ).get(i).copied().unwrap_or(0.0);
                let f = if freq > 0.0 { freq } else { root_hz };
                rate = (f as f64 / root_hz as f64) * sr_fold;
                playhead = start_norm as f64 * frames as f64;
                playing = true;
            }
            prev = g;

            let s = if playing {
                let base = playhead.floor();
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
            io.signal_mut(OUT_AUDIO)[i] = s;
        }

        self.prev_gate = prev;
        self.playhead = playhead;
        self.rate = rate;
        self.playing = playing;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        // Carry the shared resource binding forward; reset per-Lane playback state so each
        // Voice triggers independently (ADR-0016).
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

crate::register_operator!(SamplePlayer);

/// Write `n` frames of silence to the audio output.
fn silence(io: &mut Io, n: usize) {
    for s in io.signal_mut(OUT_AUDIO)[..n].iter_mut() {
        *s = 0.0;
    }
}

/// Linearly-interpolated sample at fractional position `idx + frac`, with channel select:
/// `channel < 0` averages all channels (downmix), `≥0` picks one (clamped). Pure — reads go
/// through the store's bounds-checked accessor, so out-of-range frames read as silence.
fn interp(
    store: &ResourceStore,
    id: SampleId,
    channel: f32,
    chans: usize,
    frames: usize,
    idx: usize,
    frac: f32,
) -> f32 {
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

    const SR: f32 = 48_000.0;

    /// A driver for a player bound (through the real loader path) to a one-resource store holding
    /// `buf` — exercises [`OpDriver::bind`].
    fn bound(buf: crate::resources::SampleBuffer) -> OpDriver {
        let mut d = OpDriver::for_type(SamplePlayer::new(), SR);
        d.bind("sample", buf);
        d
    }

    /// Drive `n` frames; `params` = [root, gain, start, channel] — held `Float` controls (`set`,
    /// read via `io.last`). `gate` is a `Buffer` input (`drive`n); `freq` a `Buffer` wire-in (`None`
    /// mimics an unwired buffer, which reads 0 → play at root).
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
        d.drive(IN_GATE, gate);
        if let Some(f) = freq {
            d.drive(IN_FREQ, f);
        }
        d.render(n).output(OUT_AUDIO).to_vec()
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
}
