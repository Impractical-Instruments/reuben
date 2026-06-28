//! Sample player — a one-shot trigger sampler (ADR-0016).
//!
//! The first operator to depend on **external decoded audio**: a [`ResourceStore`] built at
//! load time and bound through [`Operator::bind_resources`], read on the RT path through the
//! pure `(id, channel, frame)` accessor (bank-streaming-safe; see [`crate::resources`]).
//!
//! It slots into the **same seam as the oscillator**: it lives inside a voice sub-patch, reading
//! the voice's `freq`/`gate` Signals. Polyphony and steal-oldest come for free from the Voicer
//! hosting one sub-patch per voice (ADR-0032).
//!
//! All inputs are Value ports (ADR-0031), each owning its unwired default so `/sample/root 60` needs
//! no upstream node; each is read held (the engine block-slices at changes), with `gate` edge-detected
//! at the change frame.
//!
//! - input 0: `freq` — pitch in Hz; the playback rate is `freq / hz(root)` times the file/engine
//!   sample-rate ratio. A non-positive `freq` (the unwired default 0) → plays at `root` pitch,
//!   preserving the old "freq unconnected → root" semantics.
//! - input 1: `gate` — a **rising edge** fires the sample from `start`; one-shot plays to the buffer
//!   end ignoring release; each rising edge retriggers.
//! - input 2: `root` (MIDI) — the pitch at which the sample plays at its natural rate.
//! - input 3: `gain` (linear) — output scale.
//! - input 4: `start` (normalized 0..1) — playback start offset into the buffer.
//! - input 5: `channel` — `-1` downmixes (averages) all channels; `≥0` picks that channel. A
//!   continuous/structural selector (rounded in `process`), not an `Enum`.
//! - output 0: `audio` (`Buffer`).
//!
//! Pitch (the per-hit rate) is **latched at the trigger frame** and fixed for that hit; live
//! pitch-tracking is a deferred param. The fractional playhead is a per-voice `f64` cursor,
//! persistent across blocks (like the oscillator's phase) and reset by [`Operator::spawn`].

use std::sync::Arc;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::resources::{ResolvedRefs, ResourceStore, SampleId};

// Single-source contract (ADR-0025): one declaration -> IN_/OUT_/P_ consts + Descriptor, no drift.
crate::operator_contract!(SamplePlayer {
    type_name: "sample",
    inputs:    { freq:    f32 { 0.0..=20000.0, default 0.0, "Hz", lin },
                 gate:    f32 { 0.0..=1.0,     default 0.0, "",   lin },
                 root:    f32 { 0.0..=127.0, default 60.0, "MIDI", lin },
                 gain:    f32 { 0.0..=4.0,   default 1.0,  "",     lin },
                 start:   f32 { 0.0..=1.0,   default 0.0,  "",     lin },
                 channel: f32 { -1.0..=31.0, default -1.0, "",     lin } },
    outputs:   { audio: f32_buffer },
    resources: { sample },
});

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
        // Block-rate controls: read once at the top (ADR-0030 `Float` inputs, held via `io.input::<f32>`).
        let root_hz = Self::midi_hz(io.input::<f32>(IN_ROOT).unwrap_or(60.0));
        let gain = io.input::<f32>(IN_GAIN).unwrap_or(1.0);
        let start_norm = io.input::<f32>(IN_START).unwrap_or(0.0).clamp(0.0, 1.0);
        let channel = io.input::<f32>(IN_CHANNEL).unwrap_or(-1.0);

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

        let mut playhead = self.playhead;
        let mut rate = self.rate;
        let mut playing = self.playing;

        // `gate`/`freq` are held Values (ADR-0031): the engine block-slices at every change, so this
        // call sees one constant gate level. Detect the rising edge once at frame 0 (the slice's
        // frame 0 *is* the change frame, so the retrigger stays sample-accurate); `prev_gate` carries
        // the level across slices/blocks. Reading the held values here ends the immutable borrow
        // before the per-sample output writes below (keeps `process` alloc-free).
        let g = io.input::<f32>(IN_GATE).unwrap_or(0.0);
        if self.prev_gate <= 0.0 && g > 0.0 {
            // A non-positive `freq` (the unwired Value reads its default 0) → play at `root`,
            // preserving the old "freq unconnected → root pitch" semantics.
            let freq = io.input::<f32>(IN_FREQ).unwrap_or(0.0);
            let f = if freq > 0.0 { freq } else { root_hz };
            rate = (f as f64 / root_hz as f64) * sr_fold;
            playhead = start_norm as f64 * frames as f64;
            playing = true;
        }
        self.prev_gate = g;

        for i in 0..n {
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
            io.output::<&mut [f32]>(OUT_AUDIO)[i] = s;
        }

        self.playhead = playhead;
        self.rate = rate;
        self.playing = playing;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        // Carry the shared resource binding forward; reset per-voice playback state so each
        // voice triggers independently (ADR-0016).
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
    for s in io.output::<&mut [f32]>(OUT_AUDIO)[..n].iter_mut() {
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

    /// Drive the now-held-Value `gate` from a dense gate buffer (ADR-0031): the gate is fed by edges,
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
