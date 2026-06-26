//! Voicer — assigns incoming note Messages to Voices and emits per-Voice control Signals.
//!
//! The Voicer is the **fan-out point** (ADR-0010): it expands the Lane count to its `voices` param,
//! and the engine replicates the downstream chain once per Voice. Each replica runs the *same*
//! global voice allocation (fixed-pool, steal-oldest) over the identical note stream, and emits
//! only its own Voice's signals — so all replicas stay in lock-step and the result is deterministic.
//!
//! - input 0: `notes` (`Note`) — note events, read via [`Io::stream`]. A
//!   [`Degree`](crate::vocab::pitch::Pitch::Degree) note is resolved to Hz through the tonal context (so
//!   the line re-spells live on a key/scale change); an [`Absolute`](crate::vocab::pitch::Pitch::Absolute)
//!   note plays its MIDI coordinate. Velocity 0 is a note-off (ADR-0030: the Pitch case, not an
//!   address, carries degree-vs-absolute).
//! - input 1: `harmony` (`Harmony`, held) — the tonal context degree notes resolve against. Unconnected
//!   → the default (C major, 12-TET), so absolute-note rigs are unchanged.
//! - output 0: `freq` (`buffer`) — resolved frequency in Hz of this Voice's note.
//! - output 1: `gate` (`buffer`) — 1.0 while this Voice holds a note, else 0.0.
//! - param 0: `voices` — Voice-pool size (structural; read at Instantiate).

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::harmony::Harmony;
use crate::vocab::pitch::{Note, Pitch};

// Single-source contract (ADR-0025/0030): `notes` is a `Note` event port, `harmony` a held `Harmony`,
// `freq`/`gate` per-sample buffers; the Lane count comes from the `voices` param.
crate::operator_contract!(Voicer {
    inputs:  { notes: note, harmony: harmony },
    outputs: { freq: buffer, gate: buffer },
    params:  { voices: { 1.0..=32.0, default 8.0, "", lin } },
    lanes: from_param(voices),
});

/// Do two pitches denote the same note for note-off matching? Degrees match by degree; absolute
/// notes by MIDI. (A degree and an absolute never match — distinct identities.)
fn same_note(a: Pitch, b: Pitch) -> bool {
    match (a, b) {
        (Pitch::Degree(x), Pitch::Degree(y)) => x == y,
        (Pitch::Absolute(x), Pitch::Absolute(y)) => x == y,
        _ => false,
    }
}

/// One slot in the Voice pool.
#[derive(Clone, Copy)]
struct Voice {
    /// Symbolic pitch this Voice holds — a degree (resolved through the context, re-spells live) or
    /// an absolute MIDI note. Frequency is derived from it each block via the current context.
    pitch: Pitch,
    /// Whether the Voice is currently holding a note.
    on: bool,
    /// Assignment stamp; higher = more recently assigned (for steal-oldest).
    age: u64,
}

impl Default for Voice {
    fn default() -> Self {
        // Idle pitch = A4, so an unplayed Voice reads 440 Hz (the prior default).
        Self {
            pitch: Pitch::from_midi(69.0),
            on: false,
            age: 0,
        }
    }
}

#[derive(Default)]
pub struct Voicer {
    /// The global Voice pool, sized to the Lane count. Every replica keeps an identical copy.
    voices: Vec<Voice>,
    /// Monotonic assignment counter, for steal-oldest ordering.
    counter: u64,
}

impl Voicer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Assign `pitch` to a Voice: a free one (lowest index), else steal the oldest.
    fn assign(&mut self, pitch: Pitch) {
        let idx = self.voices.iter().position(|v| !v.on).unwrap_or_else(|| {
            self.voices
                .iter()
                .enumerate()
                .min_by_key(|(_, v)| v.age)
                .map(|(i, _)| i)
                .unwrap_or(0)
        });
        self.counter += 1;
        self.voices[idx] = Voice {
            pitch,
            on: true,
            age: self.counter,
        };
    }

    /// Release the oldest Voice currently holding `pitch`, if any.
    fn release(&mut self, pitch: Pitch) {
        if let Some(idx) = self
            .voices
            .iter()
            .enumerate()
            .filter(|(_, v)| v.on && same_note(v.pitch, pitch))
            .min_by_key(|(_, v)| v.age)
            .map(|(i, _)| i)
        {
            self.voices[idx].on = false;
        }
    }
}

impl Operator for Voicer {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let lanes = io.lanes().max(1);
        let me = io.lane().min(lanes - 1);
        // Current context (constant this segment; the engine slices at context changes, so a held
        // degree re-spells at the change frame). Default when unconnected.
        let harmony = io.last::<Harmony>(IN_HARMONY).unwrap_or_default();

        // Size the pool to the Lane count (identical across replicas).
        if self.voices.len() != lanes {
            self.voices = vec![Voice::default(); lanes];
        }

        // Snapshot note events for this (sub)block, sorted by frame. (Can't read the stream while an
        // output borrow is live, so snapshot first.) The `Note`'s Pitch carries degree-vs-absolute.
        let mut events: SmallVec<[(usize, bool, Pitch); 8]> = SmallVec::new();
        for s in io.stream::<Note>(IN_NOTES) {
            let frame = s.frame.min(n);
            events.push((frame, s.payload.velocity > 0.0, s.payload.pitch));
        }
        events.sort_by_key(|e| e.0);

        // Run the global allocation, recording only THIS Lane's change-points. Frequency is resolved
        // through the context, so a re-spell shows up as the new frame-0 value.
        let mut cur_freq = harmony.hz(self.voices[me].pitch);
        let mut cur_gate = self.voices[me].on;
        let mut changes: SmallVec<[(usize, f32, bool); 8]> = SmallVec::new();
        let (mut last_freq, mut last_gate) = (cur_freq, cur_gate);
        for &(frame, on, pitch) in &events {
            if on {
                self.assign(pitch);
            } else {
                self.release(pitch);
            }
            let v = self.voices[me];
            let f = harmony.hz(v.pitch);
            if f != last_freq || v.on != last_gate {
                changes.push((frame, f, v.on));
                last_freq = f;
                last_gate = v.on;
            }
        }

        // Fill freq, then gate (separate passes: can't hold two output borrows).
        {
            let out = io.signal_mut(OUT_FREQ);
            let mut ci = 0;
            for (i, s) in out[..n].iter_mut().enumerate() {
                while ci < changes.len() && changes[ci].0 == i {
                    cur_freq = changes[ci].1;
                    ci += 1;
                }
                *s = cur_freq;
            }
        }
        {
            let out = io.signal_mut(OUT_GATE);
            let mut ci = 0;
            for (i, s) in out[..n].iter_mut().enumerate() {
                while ci < changes.len() && changes[ci].0 == i {
                    cur_gate = changes[ci].2;
                    ci += 1;
                }
                *s = if cur_gate { 1.0 } else { 0.0 };
            }
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Voicer);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Event};
    use crate::op_driver::OpDriver;
    use crate::vocab::harmony::Harmony;

    const SR: f32 = 48_000.0;

    /// An absolute-MIDI note event: `(frame, Note(Absolute(midi), vel))`.
    fn note(midi: f32, vel: f32, frame: usize) -> (usize, Note) {
        (frame, Note::new(Pitch::Absolute(midi), vel))
    }

    /// A scale-degree note event: `(frame, Note(Degree(d), vel))`.
    fn degree(d: i32, vel: f32, frame: usize) -> (usize, Note) {
        (frame, Note::new(Pitch::Degree(d), vel))
    }

    /// Drive a fresh Voicer through the real engine and read **lane 0 / Voice 0**: `harmony` is the held
    /// `Harmony` (`set` once), `notes` are pushed `Note` events at their global frames. Returns Voice
    /// 0's (freq, gate) buffers over `n` frames (rendered as real 128-frame blocks).
    ///
    /// `OpDriver` surfaces only lane 0, and instantiates the `voices` param at its default (8), so
    /// this covers the monophonic / Voice-0 behaviors. Polyphonic Voice assignment (lanes > 0) and
    /// Voice-stealing (which needs `voices = 1/2`) stay on the hand-rolled `Io` path below — they are
    /// not expressible through `OpDriver`'s lane-0, default-param surface.
    fn drive_mono(n: usize, harmony: Harmony, events: &[(usize, Note)]) -> (Vec<f32>, Vec<f32>) {
        let mut d = OpDriver::for_type(Voicer::new(), SR);
        d.set(IN_HARMONY, harmony);
        for (frame, note) in events {
            d.push(IN_NOTES, *frame, *note);
        }
        d.render(n);
        (d.output(OUT_FREQ).to_vec(), d.output(OUT_GATE).to_vec())
    }

    /// Run one Voicer Lane over a block against `harmony`; returns its (freq, gate) buffers.
    fn run_lane_ctx(
        v: &mut Voicer,
        n: usize,
        lanes: usize,
        lane: usize,
        harmony: Harmony,
        events: &[(usize, Note)],
    ) -> (Vec<f32>, Vec<f32>) {
        let mut f = vec![0.0f32; n];
        let mut gt = vec![0.0f32; n];
        let args: Vec<Arg> = events.iter().map(|(_, nt)| Arg::Note(*nt)).collect();
        let evs: Vec<Event> = events
            .iter()
            .zip(&args)
            .map(|((frame, _), arg)| Event {
                address: "notes",
                arg,
                frame: *frame,
            })
            .collect();
        // Latch order: notes(0, placeholder — read as a stream), harmony.
        let latched = [Arg::F32(0.0), Arg::Harmony(harmony)];
        let streams: [&[Event]; 2] = [&evs, &[]];
        {
            let outs: Vec<&mut [f32]> = vec![&mut f[..], &mut gt[..]];
            let inputs: Vec<Option<&[f32]>> = vec![None, None];
            let mut io = Io::new(48_000.0, n, inputs, outs)
                .with_lane(lane, lanes)
                .with_latched(&latched)
                .with_streams(&streams);
            v.process(&mut io);
        }
        (f, gt)
    }

    fn run_lane(
        v: &mut Voicer,
        n: usize,
        lanes: usize,
        lane: usize,
        events: &[(usize, Note)],
    ) -> (Vec<f32>, Vec<f32>) {
        run_lane_ctx(v, n, lanes, lane, Harmony::default(), events)
    }

    /// Mono convenience (single Voice).
    fn run(v: &mut Voicer, n: usize, events: &[(usize, Note)]) -> (Vec<f32>, Vec<f32>) {
        run_lane(v, n, 1, 0, events)
    }

    fn hz(midi: f32) -> f32 {
        Harmony::default().hz(Pitch::from_midi(midi))
    }

    // --- monophonic behavior (Lane count 1) ---

    #[test]
    fn note_on_at_frame_zero_sets_freq_and_gate() {
        let n = 128;
        let (f, gt) = drive_mono(n, Harmony::default(), &[note(69.0, 1.0, 0)]);
        for &s in &f {
            approx::assert_relative_eq!(s, 440.0, epsilon = 1e-3);
        }
        assert!(gt.iter().all(|&g| g == 1.0));
    }

    #[test]
    fn gate_edge_is_sample_accurate() {
        let n = 128;
        let (_f, gt) = drive_mono(n, Harmony::default(), &[note(60.0, 1.0, 50)]);
        for (i, &g) in gt.iter().enumerate() {
            if i < 50 {
                assert_eq!(g, 0.0, "sample {i} should be gate-off before the note-on");
            } else {
                assert_eq!(
                    g, 1.0,
                    "sample {i} should be gate-on from the note-on onward"
                );
            }
        }
    }

    #[test]
    fn note_off_clears_gate() {
        let n = 128;
        let (_f, gt) = drive_mono(
            n,
            Harmony::default(),
            &[note(60.0, 1.0, 0), note(60.0, 0.0, 64)],
        );
        assert!(gt[..64].iter().all(|&g| g == 1.0));
        assert!(gt[64..].iter().all(|&g| g == 0.0));
    }

    // Stays on the hand-rolled `Io` path: stealing only happens when the pool is full, so it needs
    // `voices = 1`. `OpDriver` instantiates the `voices` param at its default (8), where these two
    // notes simply occupy two free Voices and Voice 0 never steals — there is no `OpDriver` surface
    // to set a param.
    #[test]
    fn one_voice_steals_so_last_note_wins() {
        let n = 128;
        let mut v = Voicer::new();
        let (f, gt) = run(&mut v, n, &[note(69.0, 1.0, 0), note(81.0, 1.0, 32)]);
        approx::assert_relative_eq!(f[0], 440.0, epsilon = 1e-3);
        approx::assert_relative_eq!(f[n - 1], 880.0, epsilon = 1e-3);
        assert!(gt.iter().all(|&g| g == 1.0));
    }

    #[test]
    fn held_note_persists_across_calls() {
        // A single 256-frame render crosses the real 128-frame block boundary at frame 128; the
        // note-on at frame 0 with no later events stays held across it (gate high, freq steady).
        let (f, gt) = drive_mono(256, Harmony::default(), &[note(69.0, 1.0, 0)]);
        assert!(
            gt.iter().all(|&g| g == 1.0),
            "held across the block boundary"
        );
        for &s in &f {
            approx::assert_relative_eq!(s, 440.0, epsilon = 1e-3);
        }
    }

    // --- polyphonic behavior (Lane count > 1) ---
    //
    // These stay on the hand-rolled `Io` path: they assert per-Voice outputs on lanes > 0 (and the
    // steal cases need a small `voices` pool). `OpDriver` drives and reads only lane 0 (Voice 0) and
    // instantiates `voices` at its default (8), so it cannot observe Voices 1.. or force a steal —
    // there is no harness surface for either. (PR2 blocker; would need `OpDriver` lane/param access.)

    /// Drive `lanes` independent replicas with the same events; return per-Lane outputs.
    fn run_poly(lanes: usize, n: usize, events: &[(usize, Note)]) -> Vec<(Vec<f32>, Vec<f32>)> {
        let mut replicas: Vec<Voicer> = (0..lanes).map(|_| Voicer::new()).collect();
        (0..lanes)
            .map(|l| run_lane(&mut replicas[l], n, lanes, l, events))
            .collect()
    }

    #[test]
    fn two_simultaneous_notes_occupy_two_voices() {
        let n = 64;
        let events = [note(60.0, 1.0, 0), note(64.0, 1.0, 0)];
        let out = run_poly(3, n, &events);
        approx::assert_relative_eq!(out[0].0[n - 1], hz(60.0), epsilon = 1e-2);
        assert!(out[0].1.iter().all(|&g| g == 1.0));
        approx::assert_relative_eq!(out[1].0[n - 1], hz(64.0), epsilon = 1e-2);
        assert!(out[1].1.iter().all(|&g| g == 1.0));
        assert!(
            out[2].1.iter().all(|&g| g == 0.0),
            "third voice should be idle"
        );
    }

    #[test]
    fn out_of_voices_steals_the_oldest() {
        let n = 64;
        // 3 notes, 2 voices: the third steals voice 0 (the oldest).
        let events = [note(60.0, 1.0, 0), note(64.0, 1.0, 10), note(67.0, 1.0, 20)];
        let out = run_poly(2, n, &events);
        approx::assert_relative_eq!(out[0].0[n - 1], hz(67.0), epsilon = 1e-2); // stolen -> 67
        approx::assert_relative_eq!(out[1].0[n - 1], hz(64.0), epsilon = 1e-2); // untouched
        assert!(out[0].1.iter().all(|&g| g == 1.0));
        assert!(out[1].1[..10].iter().all(|&g| g == 0.0));
        assert!(out[1].1[10..].iter().all(|&g| g == 1.0));
    }

    // --- degree resolution through the tonal context ---

    #[test]
    fn degree_note_resolves_through_context() {
        // Degree 4 in C major → G (MIDI 67).
        let n = 64;
        let (f, gt) = drive_mono(n, Harmony::default(), &[degree(4, 1.0, 0)]);
        approx::assert_relative_eq!(f[n - 1], hz(67.0), epsilon = 1e-2);
        assert!(gt.iter().all(|&g| g == 1.0));
    }

    // Stays on the hand-rolled `Io` path: it changes the held `harmony` *between* the note-block and a
    // later silent block with no new note. `OpDriver::set` changes a held control between `render`
    // calls, but a pushed event re-fires on every `render` (each restarts at frame 0), so a second
    // `render` would re-press degree 2 — there is no way to feed "harmony changes, no new note".
    #[test]
    fn held_degree_respells_when_context_changes() {
        // Hold degree 2. In C major it is E (64); switch the scale to C minor and the *same held
        // degree* re-spells to E♭ (63) on the next block — no new note needed.
        let n = 64;
        let mut v = Voicer::new();
        let c_major = Harmony::default();
        let (f1, _) = run_lane_ctx(&mut v, n, 1, 0, c_major, &[degree(2, 1.0, 0)]);
        approx::assert_relative_eq!(f1[n - 1], hz(64.0), epsilon = 1e-2); // E

        let c_minor = Harmony {
            scale: crate::vocab::harmony::ScaleField::new(&[0, 2, 3, 5, 7, 8, 10]),
            ..Harmony::default()
        };
        let (f2, gt2) = run_lane_ctx(&mut v, n, 1, 0, c_minor, &[]); // no new events
        approx::assert_relative_eq!(f2[n - 1], hz(63.0), epsilon = 1e-2); // E♭ — re-spelled
        assert!(gt2.iter().all(|&g| g == 1.0), "still held");
    }

    #[test]
    fn note_off_releases_only_the_matching_voice() {
        let n = 64;
        let events = [note(60.0, 1.0, 0), note(64.0, 1.0, 0), note(60.0, 0.0, 32)];
        let out = run_poly(2, n, &events);
        assert!(out[0].1[..32].iter().all(|&g| g == 1.0));
        assert!(out[0].1[32..].iter().all(|&g| g == 0.0));
        assert!(out[1].1.iter().all(|&g| g == 1.0));
    }
}
