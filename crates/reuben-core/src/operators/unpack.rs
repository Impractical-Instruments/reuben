//! `unpack` — the census of `unpack_<type>` operators.
//!
//! Each `unpack_op!` line generates one operator that **destructures a product vocab type into its
//! held fields on the wire**: it reads the whole value as a `Note` **event** stream on `in` and
//! emits each field as a held **`Value`** (the Event→Value latch). This is the one
//! greppable, auditable file of the unpackable surface — adding a product type to the
//! wire's decompose surface is a one-line edit here, no central match, `inventory` discovers the op.
//!
//! `unpack_note` is the first: it turns a `Note` event into held `pitch` (a `Pitch` leaf)
//! and `velocity` (`f32`), the operator the mono-voice unbundling test (#518) patches as
//! `unpack_note.pitch -> resolve -> osc` / `unpack_note.velocity -> envelope`.

use crate::vocab::Note;

// A `Note` unpacks to its `pitch` (a held `Pitch` leaf) and `velocity` (a held `f32`). The field
// names are the output ports verbatim; the input is the whole `Note` event on `in`.
crate::unpack_op!(Note {
    pitch: pitch,
    velocity: f32,
});

#[cfg(test)]
mod tests {
    use super::unpack_note::{IN_IN, OUT_PITCH, OUT_VELOCITY};
    use super::UnpackNote;
    use crate::message::{Arg, Emit};
    use crate::op_driver::OpDriver;
    use crate::vocab::{Note, Pitch};

    const SR: f32 = 48_000.0;

    /// The `Pitch` an emit on the `pitch` port carries.
    fn pitch_of(e: &Emit) -> Pitch {
        match &e.arg {
            Arg::Pitch(p) => *p,
            other => panic!("expected a Pitch on the pitch port, got {other:?}"),
        }
    }

    /// The velocity a `f32` emit on the `velocity` port carries.
    fn vel_of(e: &Emit) -> f32 {
        match &e.arg {
            Arg::F32(v) => *v,
            other => panic!("expected an f32 on the velocity port, got {other:?}"),
        }
    }

    /// Every pitch emit `(frame, Pitch)` across the render, in emit order.
    fn pitches(d: &OpDriver) -> Vec<(usize, Pitch)> {
        d.emits()
            .iter()
            .filter(|e| e.port == OUT_PITCH.index())
            .map(|e| (e.frame, pitch_of(e)))
            .collect()
    }

    /// Every velocity emit `(frame, f32)` across the render, in emit order.
    fn vels(d: &OpDriver) -> Vec<(usize, f32)> {
        d.emits()
            .iter()
            .filter(|e| e.port == OUT_VELOCITY.index())
            .map(|e| (e.frame, vel_of(e)))
            .collect()
    }

    // Before any event, the held fields latch to `Note::default()`: the tonic degree
    // and a note-**off** velocity, asserted at the frame-0 baseline of the first block.
    #[test]
    fn latches_to_note_default_before_the_first_event() {
        let mut d = OpDriver::for_type(UnpackNote::new(), SR);
        d.render(128);
        assert_eq!(
            pitches(&d),
            vec![(0, Pitch::Degree(0))],
            "tonic degree baseline"
        );
        assert_eq!(vels(&d), vec![(0, 0.0)], "note-off velocity baseline");
    }

    // A note mid-block sets both held fields at its frame — the Event→Value latch. The frame-0
    // baseline (the pre-event default) still precedes it.
    #[test]
    fn an_event_sets_both_held_fields_at_its_frame() {
        let mut d = OpDriver::for_type(UnpackNote::new(), SR);
        d.push(IN_IN, 64, Note::new(Pitch::from_midi(69.0), 0.9));
        d.render(128);
        assert_eq!(
            pitches(&d),
            vec![(0, Pitch::Degree(0)), (64, Pitch::Absolute(69.0))],
            "baseline then the note's pitch at its frame"
        );
        assert_eq!(
            vels(&d),
            vec![(0, 0.0), (64, 0.9)],
            "baseline then the note's velocity at its frame"
        );
    }

    // The held value carries across the block boundary (ZOH): the block after the event re-asserts
    // the last value at its frame-0 baseline, with no new event.
    #[test]
    fn holds_the_last_value_across_the_block_boundary() {
        let mut d = OpDriver::for_type(UnpackNote::new(), SR);
        d.push(IN_IN, 64, Note::new(Pitch::from_degree(4), 0.8));
        d.render(256); // two blocks; the event lands in block 0, block 1 has no event
                       // Block 1's frame-0 baseline (block-absolute frame 128) re-asserts the carried value.
        assert_eq!(
            pitches(&d),
            vec![
                (0, Pitch::Degree(0)),
                (64, Pitch::Degree(4)),
                (128, Pitch::Degree(4)),
            ],
            "the degree is held into the next block"
        );
        assert_eq!(vels(&d), vec![(0, 0.0), (64, 0.8), (128, 0.8)]);
    }

    // Two notes at the same frame resolve last-processed-wins: the held value is the
    // last event in stream order at that frame.
    #[test]
    fn simultaneous_events_are_last_processed_wins() {
        let mut d = OpDriver::for_type(UnpackNote::new(), SR);
        d.push(IN_IN, 32, Note::new(Pitch::from_degree(1), 0.5));
        d.push(IN_IN, 32, Note::new(Pitch::from_degree(5), 0.7));
        d.render(128);
        // Only the last note at frame 32 survives (MsgWriter is last-write-wins per frame).
        assert_eq!(
            pitches(&d),
            vec![(0, Pitch::Degree(0)), (32, Pitch::Degree(5))],
            "the last simultaneous note wins the held pitch"
        );
        assert_eq!(vels(&d), vec![(0, 0.0), (32, 0.7)]);
    }

    // A steady note that does not change emits nothing after its onset within a block — the
    // deduping writer keeps the wire sparse (only the onset + each block's baseline re-assertion).
    #[test]
    fn an_unchanged_value_within_a_block_does_not_re_emit() {
        let mut d = OpDriver::for_type(UnpackNote::new(), SR);
        // Two identical notes in one block: the second is a no-op change on both fields.
        d.push(IN_IN, 10, Note::new(Pitch::from_degree(2), 0.6));
        d.push(IN_IN, 90, Note::new(Pitch::from_degree(2), 0.6));
        d.render(128);
        assert_eq!(
            pitches(&d),
            vec![(0, Pitch::Degree(0)), (10, Pitch::Degree(2))],
            "the identical second note does not re-emit the pitch"
        );
        assert_eq!(vels(&d), vec![(0, 0.0), (10, 0.6)]);
    }

    // A `spawn`ed copy starts fresh — its held state is `Note::default()` again, independent of the
    // original's latched note.
    #[test]
    fn a_spawned_copy_starts_fresh() {
        let mut d = OpDriver::for_type(UnpackNote::new(), SR);
        d.push(IN_IN, 0, Note::new(Pitch::from_degree(7), 1.0));
        d.render(128); // the original now holds Degree(7)

        let mut fresh = d.spawn();
        fresh.render(128); // no event pushed to the fresh copy
        assert_eq!(
            pitches(&fresh),
            vec![(0, Pitch::Degree(0))],
            "spawn resets the held pitch to the default"
        );
        assert_eq!(vels(&fresh), vec![(0, 0.0)]);
    }
}
