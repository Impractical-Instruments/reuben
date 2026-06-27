//! Harmony — the node that owns and broadcasts the tonal context: the current [`Harmony`]
//! (key/scale/chord) (ADR-0013, ADR-0030).
//!
//! It owns the latched [`Harmony`] and publishes it onto a `harmony` output port; followers (the
//! Voicer's degree resolution, a snap op) read "what's the key/chord right now" via
//! [`Io::last::<Harmony>`]. A single default instance in a Rig makes everything agree out of the box
//! — the same on-ramp as the default Clock — without baking *global* into the core (multiple harmony
//! nodes = polytonality).
//!
//! Per-field **last-write-wins** (ADR-0013):
//! - **Static fields** — `root` and the scale (`degrees` + `s0`..`s11` step offsets) — are
//!   materialized `Float` inputs (ADR-0030; the good-button: dial the key, shape the scale). They
//!   are per-sample buffers, so `process` scans them for change frames and publishes at the exact
//!   frame — a mid-block `/harmony/root` stays sample-accurate (ADR-0015) and each can be
//!   wired/modulated.
//! - **Dynamic field** — `chord` — arrives on the held `set` (`Harmony`) input: its chord field is
//!   adopted (LWW). The chord-progression op that drives it is deferred (ADR-0030); the engine
//!   block-slices a `set` change to the segment boundary, so a chord change lands frame-accurate.
//!
//! The node publishes **on change** (emit-on-change, ADR-0015): the first block, and any block where
//! root/scale/chord differ from the last published value — so steady state is allocation-free.
//!
//! - input `set` (`Harmony`, held) — adopts its `chord` field.
//! - inputs `root`, `degrees`, `s0`..`s11` (`Float`) — the static key/scale fields.
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

/// Input ordinal of the first scale step offset; degree `k` is input `IN_S0 + k`.
pub const IN_STEP0: usize = IN_S0;

pub struct HarmonyOp {
    /// Latched chord, persisted across blocks (LWW from the `set` input's chord field).
    chord: Chord,
    /// Last value published, to publish only on change (ADR-0015). `None` until the first block,
    /// which always publishes (so the baseline picks up a non-default config).
    last: Option<Harmony>,
    /// Reused scratch for candidate publish frames, kept across blocks so steady state never
    /// reallocates. A `Float` field wired to a per-sample source can push up to `block_size - 1`
    /// change frames; a node-owned `Vec` that grows once to block size avoids an audio-thread alloc.
    frames: Vec<usize>,
}

impl Default for HarmonyOp {
    fn default() -> Self {
        Self {
            chord: Chord::empty(),
            last: None,
            frames: Vec::new(),
        }
    }
}

impl HarmonyOp {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the current context from the `Float` inputs **at frame `f`** + the latched chord. The
    /// static fields are per-sample buffers, so reading them at the change frame is what keeps a
    /// mid-block `/harmony/root` sample-accurate (ADR-0015). Falls back to a field's held default
    /// when it has no materialized buffer.
    fn current_at(&self, io: &Io, f: usize) -> Harmony {
        let at = |port| {
            io.input::<&[f32]>(port)
                .get(f)
                .copied()
                .unwrap_or_else(|| io.input::<f32>(port).unwrap_or(0.0))
        };
        let root = at(IN_ROOT).round() as i32;
        let degrees = (at(IN_DEGREES).round() as usize).clamp(1, NUM_STEPS);
        let mut offsets = [0i16; SCALE_CAP];
        for (k, o) in offsets.iter_mut().enumerate().take(degrees) {
            *o = at(IN_STEP0 + k).round() as i16;
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
        // Adopt the chord from the held `set` Harmony, if wired (the engine block-slices a change to
        // the segment boundary, so it is frame-accurate at frame 0).
        if let Some(h) = io.input::<Harmony>(IN_SET) {
            self.chord = h.chord;
        }

        // Candidate publish frames: frame 0 (first-block / cross-block change) and every interior
        // frame where a static `Float` field changes (scan only inputs that varied, so steady state
        // is free — ADR-0030). Take the reused scratch out so the publish walk can borrow `self`
        // mutably; restore it at the end, retaining capacity (alloc-free steady state).
        let n = io.frames();
        let mut frames = std::mem::take(&mut self.frames);
        frames.clear();
        frames.push(0);
        for port in [IN_ROOT, IN_DEGREES]
            .into_iter()
            .chain(IN_STEP0..IN_STEP0 + NUM_STEPS)
        {
            if io.varying(port) {
                let buf = io.input::<&[f32]>(port);
                for i in 1..buf.len().min(n) {
                    if buf[i] != buf[i - 1] {
                        frames.push(i);
                    }
                }
            }
        }
        frames.sort_unstable();
        frames.dedup();

        // Walk the frames in order: publish the resolved context if it differs from the last
        // published value.
        for &f in &frames {
            let cur = self.current_at(io, f);
            if self.last != Some(cur) {
                io.output::<Harmony>(OUT_HARMONY).set(f, cur);
                self.last = Some(cur);
            }
        }

        self.frames = frames;
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
