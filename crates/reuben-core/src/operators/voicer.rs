//! Voicer — assigns incoming note Messages to a Voice and emits control Signals.
//!
//! Monophonic for the "first sound" run (last-note priority); the fixed-pool, voice-
//! stealing polyphonic Voicer + per-Voice fan-out is the next increment (ADR-0010).
//! Ports are FROZEN (Stage A); behavior is filled test-first in Stage B.
//!
//! - input 0: `notes` (Message) — note events arrive by address routing, read via
//!   [`Io::messages`]; this port is documentary (no Signal edge).
//! - output 0: `freq` (Signal) — resolved frequency in Hz of the active note.
//! - output 1: `gate` (Signal) — 1.0 while a note is held, else 0.0.
//!
//! Note event: local address `note`, arg 0 = float MIDI note, arg 1 = velocity
//! (0 = note-off).

use crate::descriptor::{Descriptor, Port};
use crate::operator::{Io, Operator};
use crate::pitch::Pitch;
use crate::tuning::{Tuning, TwelveTet};

pub const IN_NOTES: usize = 0;
pub const OUT_FREQ: usize = 0;
pub const OUT_GATE: usize = 1;

pub struct Voicer {
    tuning: TwelveTet,
    /// Current frequency in Hz (held across blocks).
    freq: f32,
    /// Whether a note is currently held.
    gate: bool,
}

impl Default for Voicer {
    fn default() -> Self {
        Self {
            tuning: TwelveTet::default(),
            freq: 440.0,
            gate: false,
        }
    }
}

impl Voicer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Voicer {
    fn descriptor() -> Descriptor {
        Descriptor {
            type_name: "voicer",
            inputs: vec![Port::message("notes")],
            outputs: vec![Port::signal("freq"), Port::signal("gate")],
            params: vec![],
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();

        // Collect note events for this (sub)block, sorted by frame so that
        // last-note priority within the call resolves in time order. We keep a
        // small scratch of (frame, freq, gate) deltas; allocation-free for the
        // common empty case (no events), and bounded by the message count.
        // We cannot read `io.messages()` while holding an output borrow, so
        // snapshot the events first.
        let mut events: smallvec::SmallVec<[(usize, f32, bool); 8]> = smallvec::SmallVec::new();
        for msg in io.messages() {
            if msg.addr != "note" {
                continue;
            }
            let frame = msg.frame.min(n);
            let midi = match msg.args.first().and_then(crate::message::Arg::as_f32) {
                Some(v) => v,
                None => continue,
            };
            let vel = msg
                .args
                .get(1)
                .and_then(crate::message::Arg::as_f32)
                .unwrap_or(0.0);
            if vel > 0.0 {
                // Note-on: set freq, gate on.
                let hz = self.tuning.hz(Pitch::from_midi(midi));
                events.push((frame, hz, true));
            } else {
                // Note-off: keep freq, gate off.
                events.push((frame, self.freq, false));
            }
        }
        events.sort_by_key(|e| e.0);

        // Fill freq first, advancing state through events at their frames.
        let mut freq = self.freq;
        let mut gate = self.gate;
        {
            let mut ev = 0;
            let out = io.output(OUT_FREQ);
            for (i, s) in out[..n].iter_mut().enumerate() {
                while ev < events.len() && events[ev].0 == i {
                    freq = events[ev].1;
                    gate = events[ev].2;
                    ev += 1;
                }
                *s = freq;
            }
        }

        // Re-walk for gate using the same event stream (independent of freq pass).
        let mut gate_acc = self.gate;
        {
            let mut ev = 0;
            let out = io.output(OUT_GATE);
            for (i, s) in out[..n].iter_mut().enumerate() {
                while ev < events.len() && events[ev].0 == i {
                    gate_acc = events[ev].2;
                    ev += 1;
                }
                *s = if gate_acc { 1.0 } else { 0.0 };
            }
        }

        // Persist resolved state across calls.
        self.freq = freq;
        self.gate = gate;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Message};
    use crate::operator::Io;

    /// Run the voicer over one block with the given events; returns (freq, gate) buffers.
    fn run(v: &mut Voicer, n: usize, events: &[Message]) -> (Vec<f32>, Vec<f32>) {
        let mut f = vec![0.0f32; n];
        let mut gt = vec![0.0f32; n];
        {
            let mut outs: Vec<&mut [f32]> = vec![&mut f[..], &mut gt[..]];
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let mut io = Io::new(48_000.0, n, &inputs, &mut outs, &[], events);
            v.process(&mut io);
        }
        (f, gt)
    }

    #[test]
    fn note_on_at_frame_zero_sets_freq_and_gate() {
        let n = 128;
        let mut v = Voicer::new();
        let events = vec![Message::new("note", [Arg::Float(69.0), Arg::Float(1.0)], 0)];
        let (f, gt) = run(&mut v, n, &events);
        for &s in &f {
            approx::assert_relative_eq!(s, 440.0, epsilon = 1e-3);
        }
        assert!(gt.iter().all(|&g| g == 1.0));
    }

    #[test]
    fn gate_edge_is_sample_accurate() {
        let n = 128;
        let mut v = Voicer::new();
        let events = vec![Message::new(
            "note",
            [Arg::Float(60.0), Arg::Float(1.0)],
            50,
        )];
        let (_f, gt) = run(&mut v, n, &events);
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
        let mut v = Voicer::new();
        let events = vec![
            Message::new("note", [Arg::Float(60.0), Arg::Float(1.0)], 0),
            Message::new("note", [Arg::Float(60.0), Arg::Float(0.0)], 64),
        ];
        let (_f, gt) = run(&mut v, n, &events);
        assert!(gt[..64].iter().all(|&g| g == 1.0));
        assert!(gt[64..].iter().all(|&g| g == 0.0));
    }

    #[test]
    fn last_note_priority_freq_follows_second_on() {
        let n = 128;
        let mut v = Voicer::new();
        // A4 (69) then A5 (81, an octave up → 880 Hz) within one call.
        let events = vec![
            Message::new("note", [Arg::Float(69.0), Arg::Float(1.0)], 0),
            Message::new("note", [Arg::Float(81.0), Arg::Float(1.0)], 32),
        ];
        let (f, gt) = run(&mut v, n, &events);
        approx::assert_relative_eq!(f[0], 440.0, epsilon = 1e-3);
        approx::assert_relative_eq!(f[n - 1], 880.0, epsilon = 1e-3);
        assert!(gt.iter().all(|&g| g == 1.0));
    }

    #[test]
    fn held_note_persists_across_calls() {
        let n = 128;
        let mut v = Voicer::new();
        let on = vec![Message::new("note", [Arg::Float(69.0), Arg::Float(1.0)], 0)];
        let (_f1, gt1) = run(&mut v, n, &on);
        assert!(gt1.iter().all(|&g| g == 1.0));

        // Next call with no events: still held, freq unchanged.
        let (f2, gt2) = run(&mut v, n, &[]);
        assert!(gt2.iter().all(|&g| g == 1.0));
        for &s in &f2 {
            approx::assert_relative_eq!(s, 440.0, epsilon = 1e-3);
        }
    }
}
