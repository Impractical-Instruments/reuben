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
//! - input 1: `ctx` (Context) — the tonal context to snap against.
//! - output 0 (Message): `degrees` — `degree [degree, vel]` (or `note` when the target is an
//!   absolute chord with no degree); wire to a Voicer.
//! - param 0: `target` — 0 = Scale, 1 = Chord, 2 = ChordThenScale.
//! - param 1: `direction` — 0 = Nearest, 1 = Up, 2 = Down.
//!
//! Single-Lane (ADR-0014): emission is pre-fan-out.

use crate::context::{SnapDir, SnapPolicy, SnapTarget};
use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::message::Arg;
use crate::operator::{Io, Operator};

pub const IN_NOTES: usize = 0;
/// Context-input ordinal of the `ctx` port (the index [`Io::context`] uses — its own index
/// space, so the only Context input is ordinal 0, even though its full-port index is 1).
pub const IN_CTX: usize = 0;
/// Message-output ordinal of the `degrees` port (the index [`Io::emit`] uses).
pub const MSG_DEGREES: usize = 0;
pub const P_TARGET: usize = 0;
pub const P_DIRECTION: usize = 1;

#[derive(Default)]
pub struct Snap;

impl Snap {
    pub fn new() -> Self {
        Self
    }
}

fn target_of(v: f32) -> SnapTarget {
    match v.round() as i32 {
        1 => SnapTarget::Chord,
        2 => SnapTarget::ChordThenScale,
        _ => SnapTarget::Scale,
    }
}

fn direction_of(v: f32) -> SnapDir {
    match v.round() as i32 {
        1 => SnapDir::Up,
        2 => SnapDir::Down,
        _ => SnapDir::Nearest,
    }
}

impl Operator for Snap {
    fn descriptor() -> Descriptor {
        Descriptor {
            type_name: "snap",
            inputs: vec![Port::message("notes"), Port::context("ctx")],
            outputs: vec![Port::message("degrees")],
            params: vec![
                ParamMeta {
                    name: "target",
                    min: 0.0,
                    max: 2.0,
                    default: 0.0,
                    unit: "",
                    curve: Curve::Linear,
                },
                ParamMeta {
                    name: "direction",
                    min: 0.0,
                    max: 2.0,
                    default: 0.0,
                    unit: "",
                    curve: Curve::Linear,
                },
            ],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let policy = SnapPolicy {
            target: target_of(io.param(P_TARGET)),
            direction: direction_of(io.param(P_DIRECTION)),
        };
        let ctx = io.context(IN_CTX);

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
                    MSG_DEGREES,
                    "degree",
                    [Arg::Float(d as f32), Arg::Float(vel)],
                    frame,
                ),
                // An absolute (frozen-chord) target has no degree — pass the MIDI through.
                None => io.emit(
                    MSG_DEGREES,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::message::{Emit, Event, Message};

    const SR: f32 = 48_000.0;

    fn run(ctx: Context, params: &[f32], notes: &[Message]) -> Vec<Emit> {
        let evs: Vec<Event> = notes
            .iter()
            .map(|m| Event {
                addr: &m.addr,
                args: &m.args,
                frame: m.frame,
            })
            .collect();
        let ctxs = [ctx];
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let mut io = Io::new(SR, 128, inputs, outs, params, &evs)
                .with_contexts(&ctxs)
                .with_emit(&mut emits, 0);
            let mut snap = Snap::new();
            snap.process(&mut io);
        }
        emits
    }

    #[test]
    fn snaps_gesture_to_nearest_scale_degree() {
        // C major; 64.8 → F(degree 3) at Nearest (worked example §5).
        let params = vec![0.0, 0.0]; // Scale / Nearest
        let on = Message::new("note", [Arg::Float(64.8), Arg::Float(1.0)], 10);
        let emits = run(Context::default(), &params, &[on]);
        assert_eq!(emits.len(), 1);
        assert_eq!(emits[0].addr, "degree");
        assert_eq!(emits[0].frame, 10);
        approx::assert_relative_eq!(emits[0].args[0].as_f32().unwrap(), 3.0); // F = degree 3
        approx::assert_relative_eq!(emits[0].args[1].as_f32().unwrap(), 1.0); // velocity passes
    }

    #[test]
    fn already_in_scale_passes_as_its_degree() {
        let params = vec![0.0, 0.0];
        let on = Message::new("note", [Arg::Float(67.0), Arg::Float(1.0)], 0); // G = degree 4
        let emits = run(Context::default(), &params, &[on]);
        approx::assert_relative_eq!(emits[0].args[0].as_f32().unwrap(), 4.0);
    }

    #[test]
    fn non_note_events_are_ignored() {
        let params = vec![0.0, 0.0];
        let other = Message::new("chord", [Arg::Float(1.0)], 0);
        assert!(run(Context::default(), &params, &[other]).is_empty());
    }
}
