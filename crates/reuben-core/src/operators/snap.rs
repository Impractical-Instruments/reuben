//! Snap — quantizes absolute note gestures to the nearest in-scale degree (ADR-0013).
//!
//! The quantizer that sits *upstream* of resolution: an arbitrary float-MIDI gesture →
//! nearest in-scale `Pitch{degree}`, against the tonal [`Context`] it reads. It is a note
//! transformer on the internal message graph — `note` Messages in, `degree` Messages out —
//! so it composes between a note source (external play, a sequencer) and a Voicer: the
//! "always in key" good-button. Policy (target + direction) is a caller param, not baked
//! into the context, so the same context serves auto-tune (`Scale/Nearest`), an arp
//! (`Chord`), or a melody (`ChordThenScale`).
//!
//! - input 0: `notes` (Message) — absolute `note [midi, vel]` events.
//! - input 1: `ctx` (Harmony) — the tonal context to snap against.
//! - input 2: `target` (Enum {Scale, Chord, ChordThenScale}) — quantization target.
//! - input 3: `direction` (Enum {Nearest, Up, Down}) — quantization direction.
//! - output 0 (Message): `degrees` — `degree [degree, vel]` (or `note` when the target is an
//!   absolute chord with no degree); wire to a Voicer.
//!
//! Shape model (ADR-0028): `target` and `direction` are held **`Enum` inputs**, read via
//! `io.enum_index`; `ctx` is a **`Harmony` carrier** read via `io.harmony`.
//!
//! Single-Lane (ADR-0014): emission is pre-fan-out.

use crate::context::{SnapDir, SnapPolicy, SnapTarget};
use crate::descriptor::Descriptor;
use crate::message::Arg;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0028): one declaration -> IN_/OUT_ consts, the `Target`/
// `Direction` enum types, and the Descriptor; no drift.
crate::operator_contract!(Snap {
    inputs:  { notes: message,
               ctx:       context,
               target:    enum { Scale, Chord, ChordThenScale },
               direction: enum { Nearest, Up, Down } },
    outputs: { degrees: message },
});

#[derive(Default)]
pub struct Snap;

impl Snap {
    pub fn new() -> Self {
        Self
    }
}

fn target_of(t: Target) -> SnapTarget {
    match t {
        Target::Chord => SnapTarget::Chord,
        Target::ChordThenScale => SnapTarget::ChordThenScale,
        Target::Scale => SnapTarget::Scale,
    }
}

fn direction_of(d: Direction) -> SnapDir {
    match d {
        Direction::Up => SnapDir::Up,
        Direction::Down => SnapDir::Down,
        Direction::Nearest => SnapDir::Nearest,
    }
}

impl Operator for Snap {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let target = Target::from_index(io.enum_index(IN_TARGET)).unwrap_or_default();
        let direction = Direction::from_index(io.enum_index(IN_DIRECTION)).unwrap_or_default();
        let policy = SnapPolicy {
            target: target_of(target),
            direction: direction_of(direction),
        };
        let ctx = io.harmony(IN_CTX);

        // Snapshot note events (can't read events while emitting).
        let mut notes: smallvec::SmallVec<[(usize, f32, f32); 8]> = smallvec::SmallVec::new();
        for ev in io.events() {
            if ev.addr != "note" {
                continue;
            }
            let midi = match ev.args.first().and_then(Arg::as_f32) {
                Some(v) => v,
                None => continue,
            };
            let vel = ev.args.get(1).and_then(Arg::as_f32).unwrap_or(0.0);
            notes.push((ev.frame.min(io.frames()), midi, vel));
        }

        for (frame, midi, vel) in notes {
            let pitch = ctx.snap(midi, policy);
            match pitch.degree {
                Some(d) => io.emit(
                    OUT_DEGREES,
                    "degree",
                    [Arg::Float(d as f32), Arg::Float(vel)],
                    frame,
                ),
                // An absolute (frozen-chord) target has no degree — pass the MIDI through.
                None => io.emit(
                    OUT_DEGREES,
                    "note",
                    [Arg::Float(pitch.midi), Arg::Float(vel)],
                    frame,
                ),
            }
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
    use crate::context::Context;
    use crate::message::{Emit, Event, Message};

    const SR: f32 = 48_000.0;

    /// Run a fresh Snap against `ctx`. `target`/`direction` are held `Enum` inputs now (ADR-0028),
    /// supplied via `with_enums` in input-port order: notes/ctx are non-Float (slot 0), then
    /// IN_TARGET = 2 and IN_DIRECTION = 3 carry the held variant index.
    fn run(ctx: Context, target: usize, direction: usize, notes: &[Message]) -> Vec<Emit> {
        let evs: Vec<Event> = notes
            .iter()
            .map(|m| Event {
                addr: &m.addr,
                args: &m.args,
                frame: m.frame,
            })
            .collect();
        let ctxs = [ctx];
        let params: [f32; 0] = [];
        let enums = [0usize, 0, target, direction];
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let mut io = Io::new(SR, 128, inputs, outs, &params, &evs)
                .with_contexts(&ctxs)
                .with_enums(&enums)
                .with_emit(&mut emits, 0);
            let mut snap = Snap::new();
            snap.process(&mut io);
        }
        emits
    }

    #[test]
    fn snaps_gesture_to_nearest_scale_degree() {
        // C major; 64.8 → F(degree 3) at Nearest (worked example §5).
        let on = Message::new("note", [Arg::Float(64.8), Arg::Float(1.0)], 10);
        let emits = run(Context::default(), 0, 0, &[on]); // Scale / Nearest
        assert_eq!(emits.len(), 1);
        assert_eq!(emits[0].addr, "degree");
        assert_eq!(emits[0].frame, 10);
        approx::assert_relative_eq!(emits[0].args[0].as_f32().unwrap(), 3.0); // F = degree 3
        approx::assert_relative_eq!(emits[0].args[1].as_f32().unwrap(), 1.0); // velocity passes
    }

    #[test]
    fn already_in_scale_passes_as_its_degree() {
        let on = Message::new("note", [Arg::Float(67.0), Arg::Float(1.0)], 0); // G = degree 4
        let emits = run(Context::default(), 0, 0, &[on]);
        approx::assert_relative_eq!(emits[0].args[0].as_f32().unwrap(), 4.0);
    }

    #[test]
    fn non_note_events_are_ignored() {
        let other = Message::new("chord", [Arg::Float(1.0)], 0);
        assert!(run(Context::default(), 0, 0, &[other]).is_empty());
    }
}
