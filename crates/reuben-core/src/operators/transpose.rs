//! `transpose` — shift every incoming `Note` by a held amount (ADR-0030).
//!
//! The proof that the unified model carries a real two-message-input operator: a `Note` **Stream**
//! input plus a `Float` **Held** input, producing a `Note` Stream output. Each note event on
//! `notes` is shifted by the current `amount` (read via `io.last`, block-sliced at changes) and
//! re-emitted at the same frame. A scale **degree** shifts by whole steps; an **absolute** MIDI
//! pitch shifts by the same number of semitones. Velocity (and a note-off's zero velocity) carries
//! through untouched.
//!
//! - input 0: `notes` (`Note`) — incoming note events.
//! - input 1: `amount` (`Float`) — transpose amount in steps/semitones (held).
//! - output 0: `notes` (`Note`) — the shifted note events.

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::pitch::{Note, Pitch};

crate::operator_contract!(Transpose {
    inputs:  { notes:  note,
               amount: float { -48.0..=48.0, default 0.0, "steps", lin } },
    outputs: { notes: note },
});

/// Shift a note's pitch by `amount`, preserving velocity. A `Degree` shifts by whole scale steps
/// (rounded); an `Absolute` MIDI pitch by the same count of semitones.
fn transpose_note(n: Note, amount: f32) -> Note {
    let pitch = match n.pitch {
        Pitch::Degree(d) => Pitch::Degree(d + amount.round() as i32),
        Pitch::Absolute(m) => Pitch::Absolute(m + amount),
    };
    Note::new(pitch, n.velocity)
}

#[derive(Default)]
pub struct Transpose;

impl Transpose {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for Transpose {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let amount = io.last::<f32>(IN_AMOUNT).unwrap_or(0.0);

        // Snapshot the stream (its borrow of `io` ends here) so the emit loop can borrow `io`
        // mutably. `Note` is `Copy`, so this is alloc-free for the common low-event-count case.
        let mut evs: SmallVec<[(usize, Note); 16]> = SmallVec::new();
        for s in io.stream::<Note>(IN_NOTES) {
            evs.push((s.frame, s.payload));
        }
        for (frame, note) in evs {
            io.emit(OUT_NOTES, "notes", transpose_note(note, amount), frame);
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Transpose);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Emit};
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Drive a fresh Transpose through the real engine: a held `amount` (`set` once, read via
    /// `io.last`) and a set of `Note` events pushed on `notes`. Renders one block, returns the
    /// emitted Messages.
    fn run(amount: f32, notes: &[(usize, Note)]) -> Vec<Emit> {
        let mut d = OpDriver::for_type(Transpose::new(), SR);
        d.set(IN_AMOUNT, amount);
        for (frame, note) in notes {
            d.push(IN_NOTES, *frame, *note);
        }
        d.render(64).emits().to_vec()
    }

    #[test]
    fn shifts_degree_by_whole_steps() {
        let emits = run(2.0, &[(0, Note::new(Pitch::Degree(0), 1.0))]);
        assert_eq!(emits.len(), 1);
        assert_eq!(emits[0].frame, 0);
        match &emits[0].arg {
            Arg::Note(n) => {
                assert_eq!(n.pitch, Pitch::Degree(2));
                approx::assert_relative_eq!(n.velocity, 1.0);
            }
            other => panic!("expected a Note, got {other:?}"),
        }
    }

    #[test]
    fn shifts_absolute_by_semitones_and_preserves_frame_velocity() {
        let emits = run(-12.0, &[(17, Note::new(Pitch::Absolute(60.0), 0.8))]);
        assert_eq!(emits[0].frame, 17);
        match &emits[0].arg {
            Arg::Note(n) => {
                assert_eq!(n.pitch, Pitch::Absolute(48.0));
                approx::assert_relative_eq!(n.velocity, 0.8);
            }
            other => panic!("expected a Note, got {other:?}"),
        }
    }

    #[test]
    fn zero_amount_passes_notes_through() {
        let emits = run(
            0.0,
            &[
                (0, Note::new(Pitch::Degree(3), 1.0)),
                (5, Note::new(Pitch::Degree(7), 0.5)),
            ],
        );
        assert_eq!(emits.len(), 2);
        match (&emits[0].arg, &emits[1].arg) {
            (Arg::Note(a), Arg::Note(b)) => {
                assert_eq!(a.pitch, Pitch::Degree(3));
                assert_eq!(b.pitch, Pitch::Degree(7));
            }
            _ => panic!("expected two Notes"),
        }
    }
}
