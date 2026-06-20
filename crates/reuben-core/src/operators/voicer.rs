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
use crate::tuning::{TwelveTet, Tuning};

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
        // STAGE A STUB: hold last state, ignore events. Stage B implements last-note
        // priority with sample-accurate gate edges from `io.messages()`.
        let n = io.frames();
        let (freq, gate) = (self.freq, if self.gate { 1.0 } else { 0.0 });
        let _ = &self.tuning;
        // Outputs are written separately to avoid aliasing the two &mut borrows.
        io.output(OUT_FREQ)[..n].iter_mut().for_each(|s| *s = freq);
        io.output(OUT_GATE)[..n].iter_mut().for_each(|s| *s = gate);
    }
}
