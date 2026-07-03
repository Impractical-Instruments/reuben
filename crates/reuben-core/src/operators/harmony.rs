//! Harmony — the node that owns and broadcasts the tonal context: the current [`Harmony`]
//! (key/scale/chord) (ADR-0013, ADR-0030).
//!
//! It owns the latched [`Harmony`] and publishes it onto a `harmony` output port; followers (the
//! Voicer's degree resolution, a snap op) read "what's the key/chord right now" through their
//! held `harmony` input handle. A single default instance in a Rig makes everything agree out of
//! the box — the same on-ramp as the default Clock — without baking *global* into the core
//! (multiple harmony nodes = polytonality).
//!
//! Per-field **last-write-wins** (ADR-0013):
//! - **Static fields** — `root` and the scale (`degrees` + `s0`..`s11` step offsets) — are held
//!   `f32` Value inputs (ADR-0031; the good-button: dial the key, shape the scale). A mid-block
//!   change block-slices `process` at its frame, so the publish stays sample-accurate (ADR-0015).
//! - **Dynamic field** — `chord` — arrives on the held `set` (`Harmony`) input: its chord field is
//!   adopted (LWW). The chord-progression op that drives it is deferred (ADR-0030); the engine
//!   block-slices a `set` change to the segment boundary, so a chord change lands frame-accurate.
//!
//! The node publishes **on change** (emit-on-change, ADR-0015): the first block, and any (sub)block
//! where root/scale/chord differ from the last published value — so steady state is allocation-free.
//!
//! - input `set` (`Harmony`, held) — adopts its `chord` field.
//! - inputs `root`, `degrees`, `s0`..`s11` (`f32`, held) — the static key/scale fields.
//! - output `harmony` (`harmony`) — the latched tonal context followers read.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::harmony::{Chord, Harmony, ScaleField, SCALE_CAP};

/// Number of scale step-offset slots (max scale length within a 12-TET period).
pub const NUM_STEPS: usize = SCALE_CAP;

// Single-source contract (ADR-0025/0030): `set` is a held `Harmony` (its chord is adopted),
// `root`/`degrees`/`s0`..`s11` are materialized `Float` key/scale fields, `harmony` the output.
crate::operator_contract!(HarmonyOp {
    type_name: "harmony",
    inputs:  { set:     harmony,
               root:    f32 { 0.0..=127.0,      default 60.0,  "MIDI",  lin },
               degrees: f32 { 1.0..=12.0,       default 7.0,   "steps", lin },
               s0:      f32 { -24.0..=24.0,     default 0.0,   "steps", lin },
               s1:      f32 { -24.0..=24.0,     default 2.0,   "steps", lin },
               s2:      f32 { -24.0..=24.0,     default 4.0,   "steps", lin },
               s3:      f32 { -24.0..=24.0,     default 5.0,   "steps", lin },
               s4:      f32 { -24.0..=24.0,     default 7.0,   "steps", lin },
               s5:      f32 { -24.0..=24.0,     default 9.0,   "steps", lin },
               s6:      f32 { -24.0..=24.0,     default 11.0,  "steps", lin },
               s7:      f32 { -24.0..=24.0,     default 0.0,   "steps", lin },
               s8:      f32 { -24.0..=24.0,     default 0.0,   "steps", lin },
               s9:      f32 { -24.0..=24.0,     default 0.0,   "steps", lin },
               s10:     f32 { -24.0..=24.0,     default 0.0,   "steps", lin },
               s11:     f32 { -24.0..=24.0,     default 0.0,   "steps", lin } },
    outputs: { harmony: harmony },
});

/// The scale step-offset inputs in degree order — `s0`..`s11` as typed handles (ADR-0037), so a
/// loop over degrees reads through the handles the contract emitted (a computed `IN_S0 + k`
/// index would bypass the form typing).
const IN_STEPS: [crate::operator::In<crate::operator::form::Held<f32>>; NUM_STEPS] = [
    IN_S0, IN_S1, IN_S2, IN_S3, IN_S4, IN_S5, IN_S6, IN_S7, IN_S8, IN_S9, IN_S10, IN_S11,
];

pub struct HarmonyOp {
    /// Latched chord, persisted across blocks (LWW from the `set` input's chord field).
    chord: Chord,
    /// Last value published, to publish only on change (ADR-0015). `None` until the first block,
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
    /// fields are held Values (ADR-0031): a mid-block `/harmony/root` block-slices `process` at
    /// its change frame, so reading the held value once per (sub)block call is what keeps the
    /// publish sample-accurate (ADR-0015).
    fn current(&self, io: &Io) -> Harmony {
        let root = io.read(IN_ROOT).round() as i32;
        let degrees = (io.read(IN_DEGREES).round() as usize).clamp(1, NUM_STEPS);
        let mut offsets = [0i16; SCALE_CAP];
        for (k, o) in offsets.iter_mut().enumerate().take(degrees) {
            *o = io.read(IN_STEPS[k]).round() as i16;
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

        // Publish on change (ADR-0015), once per (sub)block call: every key/scale field is a
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
        d.set(IN_ROOT, 62.0); // move to D (refills the materialized `root` buffer)
        let pubs = d.render(128).emits().to_vec();
        assert_eq!(pubs.len(), 1);
        assert_eq!(harmony_of(&pubs[0]).root, 62);
    }
}
