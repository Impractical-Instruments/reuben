//! Snap — quantizes absolute note gestures to the nearest in-scale degree.
//!
//! The quantizer that sits *upstream* of resolution: an arbitrary absolute (float-MIDI) note →
//! nearest in-scale `Pitch::Degree`, against the tonal [`Harmony`](crate::vocab::harmony::Harmony) it
//! reads. It is a note transformer on the internal message graph — `Note` events in, `Note`
//! events out — so it composes between a note source (external play, a sequencer) and a Voicer:
//! the "always in key" good-button. Policy (target + direction) is a held caller input, not baked
//! into the context, so the same context serves auto-tune (`Scale`/`Nearest`), an arp (`Chord`),
//! or a melody (`ChordThenScale`).
//!
//! - input 0: `notes` (`Note`) — incoming note events; an [`Absolute`](crate::vocab::pitch::Pitch::Absolute)
//!   pitch is snapped, a [`Degree`](crate::vocab::pitch::Pitch::Degree) pitch is already in-scale and is
//!   passed through (the Pitch case, not an address, carries the distinction).
//! - input 1: `harmony` (`Harmony`, held) — the tonal context to snap against.
//! - input 2: `target` (`enum` [`SnapTarget`]) — quantization target.
//! - input 3: `direction` (`enum` [`SnapDir`]) — quantization direction.
//! - output 0: `notes` (`Note`) — the snapped note (a [`Degree`](crate::vocab::pitch::Pitch::Degree)
//!   where possible, an [`Absolute`](crate::vocab::pitch::Pitch::Absolute) when a frozen-chord target has
//!   no degree); wire to a Voicer.
//!
//! Emits one note stream, upstream of the Voicer that fans it out to voices.

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::harmony::SnapPolicy;
use crate::vocab::pitch::{Note, Pitch};

// `target`/`direction` reference the shared `SnapTarget`/
// `SnapDir` vocab enums; `harmony` is the held `Harmony` carrier.
crate::operator_contract!(Snap {
    inputs:  { notes:     note,
               harmony:   harmony,
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
            target: io.read(IN_TARGET),
            direction: io.read(IN_DIRECTION),
        };
        let harmony = io.read(IN_HARMONY);

        // Snapshot incoming notes (its borrow of `io` ends here) so the emit loop can borrow `io`
        // mutably. `Note` is `Copy`, so this is alloc-free for the common low-event-count case.
        let mut notes: SmallVec<[(usize, Note); 8]> = SmallVec::new();
        for s in io.read(IN_NOTES) {
            notes.push((s.frame, s.payload));
        }

        for (frame, note) in notes {
            // Snap only operates on an absolute pitch; a degree is already in-scale, pass it through.
            let pitch = match note.pitch {
                Pitch::Absolute(midi) => harmony.snap(midi, policy),
                Pitch::Degree(_) => note.pitch,
            };
            io.write(OUT_NOTES)
                .emit(frame, Note::new(pitch, note.velocity));
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
    use crate::message::{Arg, Emit};
    use crate::op_driver::OpDriver;
    use crate::vocab::harmony::{Chord, ChordTag, Harmony, SnapDir, SnapTarget};

    const SR: f32 = 48_000.0;

    /// Drive a fresh Snap against `harmony` through the real engine. `target`/`direction` are held vocab
    /// enums and `harmony` a held `Harmony` (all `set` once); `notes` are pushed as `Note` events. Renders
    /// one block and returns the emitted Messages.
    fn run(
        harmony: Harmony,
        target: SnapTarget,
        direction: SnapDir,
        notes: &[(usize, Note)],
    ) -> Vec<Emit> {
        let mut d = OpDriver::for_type(Snap::new(), SR);
        d.set(IN_HARMONY, harmony)
            .set(IN_TARGET, target)
            .set(IN_DIRECTION, direction);
        for (frame, note) in notes {
            d.push(IN_NOTES, *frame, *note);
        }
        d.render(128).emits().to_vec()
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
    fn non_default_policy_reaches_the_snapper() {
        // Every other test uses both enums' `#[default]` variants (Scale/Nearest), so none can
        // tell reading the `target`/`direction` ports apart from `SnapPolicy::default()` — or
        // from a mis-indexed port whose failed enum decode falls back to the handle default.
        // Pin one non-default value per port, cross-checked against the vocab-level snap tests.
        //
        // Non-default direction: 64.8 in default C major snaps to F (degree 3) at Nearest
        // (pinned by snaps_gesture_to_nearest_scale_degree); a held SnapDir::Down must reach
        // Harmony::snap and give E (degree 2).
        let on = (0, Note::new(Pitch::Absolute(64.8), 1.0));
        let emits = run(Harmony::default(), SnapTarget::Scale, SnapDir::Down, &[on]);
        assert_eq!(degree(&emits[0]), 2); // E, not Nearest's F

        // Non-default target: strict Chord over C-E-G (scale-relative {0,2,4}) snaps 65.0 (F,
        // a scale tone Scale/Nearest would keep) to the nearest chord tone E — mirrors the
        // vocab-level chord-target case, but through the operator's ports.
        let c = Harmony {
            chord: Chord::new(ChordTag::ScaleRelative, &[0, 2, 4]),
            ..Harmony::default()
        };
        let on = (0, Note::new(Pitch::Absolute(65.0), 1.0));
        let emits = run(c, SnapTarget::Chord, SnapDir::Nearest, &[on]);
        assert_eq!(c.degree_to_step(degree(&emits[0])), 64); // E = MIDI 64
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
