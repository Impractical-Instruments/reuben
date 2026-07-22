//! Harmony — the node that owns and broadcasts the tonal context: the current [`Harmony`]
//! (key/scale/chord).
//!
//! It owns the latched [`Harmony`] and publishes it onto a `harmony` output port; followers (the
//! Voicer's degree resolution, a snap op) read "what's the key/chord right now" through their
//! held `harmony` input handle. A single default instance in a Rig makes everything agree out of
//! the box — the same on-ramp as the default Clock — without baking *global* into the core
//! (multiple harmony nodes = polytonality).
//!
//! Per-field **last-write-wins**:
//! - **Static fields** — `root` and the scale (`degrees` + `s0`..`s11` step offsets) — are held
//!   `i32` Value inputs (the good-button: dial the key, shape the scale). They are integer
//!   quantities by construction — a MIDI root, a degree count, and step offsets into an integer
//!   step-space (microtonality lives in the [`Tuning`](crate::vocab::tuning) layer, not here) — so
//!   `i32` carries them without the round-in-`process` dance. A mid-block change block-slices
//!   `process` at its frame, so the publish stays sample-accurate.
//! - **Dynamic field** — `chord` — arrives on the held `set` (`Harmony`) input: its chord field is
//!   adopted (LWW). The engine block-slices a `set` change to the segment boundary, so a chord
//!   change lands frame-accurate.
//!
//! The node publishes **on change**: the first block, and any (sub)block where root/scale/chord
//! differ from the last published value — so steady state is allocation-free.
//!
//! - input `set` (`Harmony`, held) — adopts its `chord` field.
//! - inputs `root`, `degrees`, `s0`..`s11` (`i32`, held) — the static key/scale fields.
//! - output `harmony` (`harmony`) — the latched tonal context followers read.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::harmony::{Chord, Harmony, ScaleField, SCALE_CAP};

/// Number of scale step-offset slots (max scale length within a 12-TET period).
pub const NUM_STEPS: usize = SCALE_CAP;

// The `degrees` port's contract max is the literal `12`; keep it tied to `SCALE_CAP` (the
// `offsets` array length the `degrees` cap keeps `offsets[..degrees]` in bounds against) so a
// change to one without the other fails to compile.
const _: () = assert!(NUM_STEPS == 12);

// `set` is a held `Harmony` (its chord is adopted), `root`/`degrees`/`s0`..`s11` are held `i32`
// key/scale fields, `harmony` the output. `root` (MIDI) and `degrees` (count) carry their static
// bounds in the port contract; the scale offsets are integer indices into step-space.
crate::operator_contract!(HarmonyOp {
    type_name: "harmony",
    inputs:  { set:     harmony,
               root:    i32 { 0..=127, default 60 },
               degrees: i32 { 1..=12,  default 7 },
               s0:      i32 { -24..=24, default 0 },
               s1:      i32 { -24..=24, default 2 },
               s2:      i32 { -24..=24, default 4 },
               s3:      i32 { -24..=24, default 5 },
               s4:      i32 { -24..=24, default 7 },
               s5:      i32 { -24..=24, default 9 },
               s6:      i32 { -24..=24, default 11 },
               s7:      i32 { -24..=24, default 0 },
               s8:      i32 { -24..=24, default 0 },
               s9:      i32 { -24..=24, default 0 },
               s10:     i32 { -24..=24, default 0 },
               s11:     i32 { -24..=24, default 0 } },
    outputs: { harmony: harmony },
});

/// The scale step-offset inputs in degree order — `s0`..`s11` as typed handles, so a
/// loop over degrees reads through the handles the contract emitted (a computed `IN_S0 + k`
/// index would bypass the form typing).
const IN_STEPS: [crate::operator::In<crate::operator::form::Held<i32>>; NUM_STEPS] = [
    IN_S0, IN_S1, IN_S2, IN_S3, IN_S4, IN_S5, IN_S6, IN_S7, IN_S8, IN_S9, IN_S10, IN_S11,
];

pub struct HarmonyOp {
    /// Latched chord, persisted across blocks (LWW from the `set` input's chord field).
    chord: Chord,
    /// Last value published, to publish only on change. `None` until the first block,
    /// which always publishes (so the baseline picks up a non-default config).
    last: Option<Harmony>,
}

impl Default for HarmonyOp {
    fn default() -> Self {
        Self {
            chord: Chord::empty(),
            last: None,
        }
    }
}

impl HarmonyOp {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the current context from the held key/scale inputs + the latched chord. The static
    /// fields are held Values: a mid-block `/harmony/root` block-slices `process` at
    /// its change frame, so reading the held value once per (sub)block call is what keeps the
    /// publish sample-accurate.
    fn current(&self, io: &Io) -> Harmony {
        // Held `i32` fields: `root` (MIDI) and `degrees` (count) carry their full range in the port
        // contract. `degrees` indexes the fixed `offsets` array, so here we keep the two totality
        // guards the hot path needs — a `1` floor and a `SCALE_CAP` cap — so `offsets[..degrees]`
        // can never panic even on the `OpDriver` path that writes the latch raw (a negative latch
        // would otherwise wrap to a huge `usize`; the hot-path-totality rule). The step offsets fit
        // `i16` (`-24..=24`).
        let root = io.read(IN_ROOT);
        let degrees = (io.read(IN_DEGREES).max(1) as usize).min(SCALE_CAP);
        let mut offsets = [0i16; SCALE_CAP];
        for (k, o) in offsets.iter_mut().enumerate().take(degrees) {
            *o = io.read(IN_STEPS[k]) as i16;
        }
        Harmony {
            root,
            scale: ScaleField::new(&offsets[..degrees]),
            chord: self.chord,
        }
    }
}

impl Operator for HarmonyOp {
    fn descriptor() -> Descriptor {
        // Default scale = C major; root = middle C (the per-input defaults). So the default context
        // equals the engine default (existing rigs unchanged).
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        // Adopt the chord from the held `set` Harmony (LWW). The engine seeds an unwired port
        // with the default `Harmony` (empty chord), so this read is total; a change block-slices
        // to the segment boundary, landing frame-accurate at this call's frame 0.
        self.chord = io.read(IN_SET).chord;

        // Publish on change, once per (sub)block call: every key/scale field is a
        // held Value, so any mid-block change starts a new slice — frame 0 of *this* call is the
        // change frame, and the `MsgWriter` stamps it block-absolute. Steady state emits nothing
        // (the value compare short-circuits the deduping writer).
        let cur = self.current(io);
        if self.last != Some(cur) {
            io.write(OUT_HARMONY).set(0, cur);
            self.last = Some(cur);
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }

    fn on_transplant(&mut self) {
        // Surviving a Swap, this box kept its emit-on-change baseline, but the downstream `harmony`
        // consumer latch was rebuilt to Harmony::default(). Clear the baseline so the
        // first post-swap block re-publishes the current context — otherwise the voice it drives is
        // silently retransposed onto the default (C major, root 60). The latched `chord` is real
        // survivor state and is preserved; only the publish baseline resets. RT-safe: one field write.
        self.last = None;
    }
}

crate::register_operator!(HarmonyOp);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Emit};
    use crate::op_driver::OpDriver;
    use crate::vocab::harmony::{Chord as HChord, ChordTag};

    const SR: f32 = 48_000.0;

    fn harmony_of(e: &Emit) -> Harmony {
        match &e.arg {
            Arg::Harmony(h) => *h,
            other => panic!("expected a Harmony, got {other:?}"),
        }
    }

    #[test]
    fn publishes_default_once_then_stays_quiet() {
        // A fresh driver seeds the materialized `root`/`degrees`/`s0..s11` Float inputs from the
        // contract defaults (the same per-node seeding the engine builds), so the first block
        // publishes the default context.
        let mut d = OpDriver::for_type(HarmonyOp::new(), SR);
        let first = d.render(128).emits().to_vec();
        assert_eq!(first.len(), 1, "first block publishes the initial context");
        assert_eq!(first[0].frame, 0);
        assert_eq!(harmony_of(&first[0]), Harmony::default());
        // No change → no further publishes.
        let second = d.render(128).emits().to_vec();
        assert!(second.is_empty(), "unchanged context does not re-publish");
    }

    #[test]
    fn on_transplant_re_publishes_the_current_context_not_the_default() {
        // A Swap transplants this surviving `harmony` box while the downstream consumer's held
        // `harmony` latch is rebuilt to Harmony::default() (C major, root 60). Because the op
        // publishes on change against a baseline in its box, a plain post-swap block
        // would see no change and stay silent — stranding the consumer on that default and silently
        // retransposing the voice. `on_transplant` must clear the baseline so the first post-swap
        // block re-asserts the *current* context (here: root 45, natural minor), not the default.
        let mut d = OpDriver::for_type(HarmonyOp::new(), SR);
        let _ = d.render(128); // consume the initial default publish
        d.set(IN_ROOT, 45)
            .set(IN_S2, 3) // natural-minor third, so the scale differs from the C-major default
            .set(IN_S5, 8)
            .set(IN_S6, 10);
        let moved = d.render(128).emits().to_vec();
        assert_eq!(moved.len(), 1, "the root/scale move publishes once");
        assert_eq!(harmony_of(&moved[0]).root, 45);
        // Steady state: unchanged context, no publish.
        assert!(
            d.render(128).emits().is_empty(),
            "an unchanged context does not re-publish"
        );

        // Now the Swap seam: transplant, then render one block with NO input change.
        d.on_transplant();
        let after = d.render(128).emits().to_vec();
        assert_eq!(
            after.len(),
            1,
            "on_transplant forces a re-assertion on the next block despite no input change"
        );
        let ctx = harmony_of(&after[0]);
        assert_eq!(
            ctx.root, 45,
            "and it re-asserts the current root, not the default 60"
        );
        assert_eq!(
            ctx,
            Harmony {
                root: 45,
                scale: ScaleField::new(&[0, 2, 3, 5, 7, 8, 10]),
                chord: Chord::empty(),
            },
            "the full current context is re-asserted, not Harmony::default()"
        );
    }

    #[test]
    fn negative_degrees_degrades_without_panicking() {
        // A raw negative `degrees` latch — the `OpDriver`/bench path bypasses the port's `1..`
        // floor — would wrap to a huge `usize` and panic the `offsets[..degrees]` slice on the
        // render thread. `process` floors at 1 and caps at `SCALE_CAP`, so this degrades to a
        // 1-degree scale; the point is it renders at all.
        let mut d = OpDriver::for_type(HarmonyOp::new(), SR);
        let _ = d.render(128); // consume the default 7-degree publish
        d.set(IN_DEGREES, -1);
        let pubs = d.render(128).emits().to_vec();
        assert_eq!(pubs.len(), 1, "the degrees change publishes once, no panic");
        assert_eq!(harmony_of(&pubs[0]).scale, ScaleField::new(&[0]));
    }

    #[test]
    fn chord_from_set_publishes() {
        let mut d = OpDriver::for_type(HarmonyOp::new(), SR);
        let _ = d.render(128); // consume the initial publish
        let with_chord = Harmony {
            chord: HChord::new(ChordTag::ScaleRelative, &[0, 2, 4]),
            ..Harmony::default()
        };
        d.set(IN_SET, with_chord);
        let pubs = d.render(128).emits().to_vec();
        assert_eq!(pubs.len(), 1);
        assert_eq!(pubs[0].frame, 0);
        assert_eq!(harmony_of(&pubs[0]).chord.tag, ChordTag::ScaleRelative);
    }

    #[test]
    fn root_change_publishes() {
        let mut d = OpDriver::for_type(HarmonyOp::new(), SR);
        let _ = d.render(128);
        d.set(IN_ROOT, 62); // move to D
        let pubs = d.render(128).emits().to_vec();
        assert_eq!(pubs.len(), 1);
        assert_eq!(harmony_of(&pubs[0]).root, 62);
    }

    #[test]
    fn scale_shape_change_publishes_the_new_scale() {
        // The scale shape (`degrees` + `s0`..`s11`) is read through the hand-ordered IN_STEPS
        // handle table: any two swapped entries — or an off-by-one against the contract's port
        // numbering — would publish a scrambled scale while the root/chord tests stay green.
        // All five pentatonic offsets are distinct, so the whole-Harmony compare below is
        // sensitive to any single-pair swap; `s5 = 24` proves slots past `degrees` are
        // truncated, never leaked into the published scale.
        let mut d = OpDriver::for_type(HarmonyOp::new(), SR);
        let _ = d.render(128); // consume the initial default publish
        d.set(IN_DEGREES, 5)
            .set(IN_S0, 0)
            .set(IN_S1, 3)
            .set(IN_S2, 5)
            .set(IN_S3, 7)
            .set(IN_S4, 10)
            .set(IN_S5, 24); // past `degrees` — must not reach the scale
        let pubs = d.render(128).emits().to_vec();
        assert_eq!(pubs.len(), 1, "one publish for the scale change");
        assert_eq!(pubs[0].frame, 0);
        assert_eq!(
            harmony_of(&pubs[0]),
            Harmony {
                root: 60, // untouched static fields keep their defaults
                scale: ScaleField::new(&[0, 3, 5, 7, 10]),
                chord: Chord::empty(),
            }
        );
        // Emit-on-change covers the scale fields too, not just root/chord.
        let quiet = d.render(128).emits().to_vec();
        assert!(quiet.is_empty(), "unchanged scale does not re-publish");
        // A 0-degree request still publishes a 1-degree scale: `ScaleField::new` floors the length
        // at 1 (the `I32Meta` `1..=12` bound clamps live input up front in the engine; here the
        // floor is the last line of defence).
        d.set(IN_DEGREES, 0);
        let pubs = d.render(128).emits().to_vec();
        assert_eq!(pubs.len(), 1);
        assert_eq!(harmony_of(&pubs[0]).scale, ScaleField::new(&[0]));
    }
}
