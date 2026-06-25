//! Snap — quantizes absolute note gestures to the nearest in-scale degree (ADR-0013, ADR-0030).
//!
//! The quantizer that sits *upstream* of resolution: an arbitrary absolute (float-MIDI) note →
//! nearest in-scale `Pitch::Degree`, against the tonal [`Harmony`](crate::harmony::Harmony) it
//! reads. It is a note transformer on the internal message graph — `Note` events in, `Note`
//! events out — so it composes between a note source (external play, a sequencer) and a Voicer:
//! the "always in key" good-button. Policy (target + direction) is a held caller input, not baked
//! into the context, so the same context serves auto-tune (`Scale`/`Nearest`), an arp (`Chord`),
//! or a melody (`ChordThenScale`).
//!
//! - input 0: `notes` (`Note`) — incoming note events; an [`Absolute`](crate::pitch::Pitch::Absolute)
//!   pitch is snapped, a [`Degree`](crate::pitch::Pitch::Degree) pitch is already in-scale and is
//!   passed through (ADR-0030: the Pitch case, not an address, carries the distinction).
//! - input 1: `ctx` (`Harmony`, held) — the tonal context to snap against.
//! - input 2: `target` (`enum` [`SnapTarget`]) — quantization target.
//! - input 3: `direction` (`enum` [`SnapDir`]) — quantization direction.
//! - output 0: `notes` (`Note`) — the snapped note (a [`Degree`](crate::pitch::Pitch::Degree)
//!   where possible, an [`Absolute`](crate::pitch::Pitch::Absolute) when a frozen-chord target has
//!   no degree); wire to a Voicer.
//!
//! Single-Lane (ADR-0014): emission is pre-fan-out.

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::harmony::{Harmony, SnapDir, SnapPolicy, SnapTarget};
use crate::operator::{Io, Operator};
use crate::pitch::{Note, Pitch};

// Single-source contract (ADR-0025/0030). `target`/`direction` reference the shared `SnapTarget`/
// `SnapDir` vocab enums; `ctx` is the held `Harmony` carrier.
crate::operator_contract!(Snap {
    inputs:  { notes:     note,
               ctx:       harmony,
               target:    enum(SnapTarget),
               direction: enum(SnapDir) },
    outputs: { notes: note },
});

#[derive(Default)]
pub struct Snap;

impl Snap {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for Snap {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let policy = SnapPolicy {
            target: io.last::<SnapTarget>(IN_TARGET).unwrap_or_default(),
            direction: io.last::<SnapDir>(IN_DIRECTION).unwrap_or_default(),
        };
        let ctx = io.last::<Harmony>(IN_CTX).unwrap_or_default();

        // Snapshot incoming notes (its borrow of `io` ends here) so the emit loop can borrow `io`
        // mutably. `Note` is `Copy`, so this is alloc-free for the common low-event-count case.
        let mut notes: SmallVec<[(usize, Note); 8]> = SmallVec::new();
        for s in io.stream::<Note>(IN_NOTES) {
            notes.push((s.frame, s.payload));
        }

        for (frame, note) in notes {
            // Snap only operates on an absolute pitch; a degree is already in-scale, pass it through.
            let pitch = match note.pitch {
                Pitch::Absolute(midi) => ctx.snap(midi, policy),
                Pitch::Degree(_) => note.pitch,
            };
            io.emit(OUT_NOTES, "notes", Note::new(pitch, note.velocity), frame);
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Snap);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Emit, Event};

    const SR: f32 = 48_000.0;

    /// Run a fresh Snap against `ctx`. `target`/`direction` are held vocab enums; `ctx` a held
    /// `Harmony`; `notes` arrives as a `Note` stream. Returns the emitted Messages.
    fn run(
        ctx: Harmony,
        target: SnapTarget,
        direction: SnapDir,
        notes: &[(usize, Note)],
    ) -> Vec<Emit> {
        let args: Vec<Arg> = notes.iter().map(|(_, n)| Arg::Note(*n)).collect();
        let evs: Vec<Event> = notes
            .iter()
            .zip(&args)
            .map(|((frame, _), arg)| Event {
                address: "notes",
                arg,
                frame: *frame,
            })
            .collect();
        // Latch order: notes(0, placeholder — read as a stream), ctx, target, direction.
        let latched = [
            Arg::F32(0.0),
            Arg::Harmony(ctx),
            Arg::SnapTarget(target),
            Arg::SnapDir(direction),
        ];
        let streams: [&[Event]; 4] = [&evs, &[], &[], &[]];
        let mut emits: Vec<Emit> = Vec::new();
        {
            let inputs: Vec<Option<&[f32]>> = vec![None, None, None, None];
            let outs: Vec<&mut [f32]> = vec![];
            let mut io = Io::new(SR, 128, inputs, outs)
                .with_latched(&latched)
                .with_streams(&streams)
                .with_emit(&mut emits, 0);
            Snap::new().process(&mut io);
        }
        emits
    }

    fn degree(e: &Emit) -> i32 {
        match &e.arg {
            Arg::Note(n) => n.pitch.degree().unwrap(),
            other => panic!("expected a Note, got {other:?}"),
        }
    }
    fn vel(e: &Emit) -> f32 {
        match &e.arg {
            Arg::Note(n) => n.velocity,
            other => panic!("expected a Note, got {other:?}"),
        }
    }

    #[test]
    fn snaps_gesture_to_nearest_scale_degree() {
        // C major; 64.8 → F (degree 3) at Nearest (worked example §5).
        let on = (10, Note::new(Pitch::Absolute(64.8), 1.0));
        let emits = run(
            Harmony::default(),
            SnapTarget::Scale,
            SnapDir::Nearest,
            &[on],
        );
        assert_eq!(emits.len(), 1);
        assert_eq!(emits[0].frame, 10);
        assert_eq!(degree(&emits[0]), 3); // F = degree 3
        approx::assert_relative_eq!(vel(&emits[0]), 1.0); // velocity passes
    }

    #[test]
    fn already_in_scale_passes_as_its_degree() {
        let on = (0, Note::new(Pitch::Absolute(67.0), 1.0)); // G = degree 4
        let emits = run(
            Harmony::default(),
            SnapTarget::Scale,
            SnapDir::Nearest,
            &[on],
        );
        assert_eq!(degree(&emits[0]), 4);
    }

    #[test]
    fn degree_note_passes_through_unchanged() {
        // A degree note is already in-scale: it passes through untouched (no snapping).
        let on = (5, Note::new(Pitch::Degree(2), 0.9));
        let emits = run(
            Harmony::default(),
            SnapTarget::Scale,
            SnapDir::Nearest,
            &[on],
        );
        assert_eq!(emits.len(), 1);
        assert_eq!(degree(&emits[0]), 2);
        approx::assert_relative_eq!(vel(&emits[0]), 0.9);
    }
}
