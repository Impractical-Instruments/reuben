//! Context — the tonal-context node: broadcasts the current key/scale/chord (ADR-0013).
//!
//! It owns the latched [`Context`] and publishes it onto a Context output port; followers
//! (the Voicer's degree resolution, a snap op) read "what's the key/chord right now" via
//! `io.context()`. A single default instance in a Rig makes everything agree out of the box
//! — the same on-ramp as the default Clock — without baking *global* into the core
//! (multiple context nodes = polytonality).
//!
//! Per-field **last-write-wins** (ADR-0013):
//! - **Static fields** — `root` and the scale (`degrees` + `s0`..`s11` step offsets) — are
//!   ordinary f32 params (the good-button: dial the key, shape the scale), so an external
//!   `/context/root` or `/context/s2` is sample-accurate via block-slicing. (Named scales
//!   like `"dorian"` and non-12-TET tunings are a deferred preset/registry layer; the scale
//!   here is the explicit step-offset list ADR-0013 specifies.)
//! - **Dynamic field** — `chord` — arrives as a `chord` Message on the `set` input
//!   (`/context/chord [tag, d0, d1, …]`, `tag`: 0 = clear, 1 = scale-relative, 2 = absolute).
//!
//! The node publishes **on change** (emit-on-change, ADR-0015): the first block, and any
//! block where root/scale/chord differ from the last published value — so steady state is
//! allocation-free. A chord change mid-block publishes at the change frame, so it is
//! sample-accurate on the same timeline as notes.
//!
//! - input 0: `set` (Message) — `chord` writes (also reachable from an internal
//!   chord-progression op wired to this port; that op is deferred).
//! - output 0: `ctx` (Context) — the latched context followers read.

use smallvec::SmallVec;

use crate::context::{Chord, ChordTag, Context, ScaleField, CHORD_CAP, SCALE_CAP};
use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::message::Arg;
use crate::operator::{Io, Operator};

pub const IN_SET: usize = 0;
/// Context-output ordinal of the `ctx` port (the index [`Io::publish_context`] uses).
pub const CTX_OUT: usize = 0;
pub const P_ROOT: usize = 0;
pub const P_DEGREES: usize = 1;
/// Slot of the first scale step offset; degree `k` is param `P_STEP0 + k`.
pub const P_STEP0: usize = 2;
/// Number of scale step-offset slots (max scale length within a 12-TET period).
pub const NUM_STEPS: usize = SCALE_CAP;

pub struct ContextOp {
    /// Latched chord, persisted across blocks (LWW from `chord` writes).
    chord: Chord,
    /// Last value published, to publish only on change (ADR-0015). `None` until the first
    /// block, which always publishes (so the baseline picks up a non-default config).
    last: Option<Context>,
}

impl Default for ContextOp {
    fn default() -> Self {
        Self {
            chord: Chord::empty(),
            last: None,
        }
    }
}

impl ContextOp {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the current context from this segment's (constant) params + latched chord.
    fn current(&self, io: &Io) -> Context {
        let root = io.param(P_ROOT).round() as i32;
        let degrees = (io.param(P_DEGREES).round() as usize).clamp(1, NUM_STEPS);
        let mut offsets = [0i16; SCALE_CAP];
        for (k, o) in offsets.iter_mut().enumerate().take(degrees) {
            *o = io.param(P_STEP0 + k).round() as i16;
        }
        Context {
            root,
            scale: ScaleField::new(&offsets[..degrees]),
            chord: self.chord,
        }
    }
}

impl Operator for ContextOp {
    fn descriptor() -> Descriptor {
        // Default scale = C major; root = middle C. So the default context equals the engine
        // default (existing rigs unchanged).
        const DEFAULT_OFFSETS: [f32; NUM_STEPS] =
            [0.0, 2.0, 4.0, 5.0, 7.0, 9.0, 11.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut params = Vec::with_capacity(NUM_STEPS + 2);
        params.push(ParamMeta {
            name: "root",
            min: 0.0,
            max: 127.0,
            default: 60.0,
            unit: "MIDI",
            curve: Curve::Linear,
        });
        params.push(ParamMeta {
            name: "degrees",
            min: 1.0,
            max: NUM_STEPS as f32,
            default: 7.0,
            unit: "steps",
            curve: Curve::Linear,
        });
        const STEP_NAMES: [&str; NUM_STEPS] = [
            "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10", "s11",
        ];
        for (name, default) in STEP_NAMES.iter().zip(DEFAULT_OFFSETS) {
            params.push(ParamMeta {
                name,
                min: -24.0,
                max: 24.0,
                default,
                unit: "steps",
                curve: Curve::Linear,
            });
        }
        Descriptor {
            type_name: "context",
            inputs: vec![Port::message("set")],
            outputs: vec![Port::context("ctx")],
            params,
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        // Snapshot chord writes for this (sub)block, sorted by frame. (Can't read events while
        // publishing.) A `chord` event: arg0 = tag (0 clear / 1 scale-relative / 2 absolute),
        // args1.. = offsets (degrees if scale-relative, steps if absolute).
        let mut writes: SmallVec<[(usize, Chord); 4]> = SmallVec::new();
        for ev in io.events() {
            if ev.addr != "chord" {
                continue;
            }
            let tag = ev.args.first().and_then(Arg::as_f32).unwrap_or(0.0).round() as i32;
            let chord = match tag {
                1 | 2 => {
                    let mut offs = [0i16; CHORD_CAP];
                    let mut n = 0;
                    for a in ev.args.iter().skip(1).take(CHORD_CAP) {
                        if let Some(v) = a.as_f32() {
                            offs[n] = v.round() as i16;
                            n += 1;
                        }
                    }
                    let t = if tag == 1 {
                        ChordTag::ScaleRelative
                    } else {
                        ChordTag::Absolute
                    };
                    Chord::new(t, &offs[..n])
                }
                _ => Chord::empty(),
            };
            writes.push((ev.frame.min(io.frames()), chord));
        }
        writes.sort_by_key(|w| w.0);

        // Publish at frame 0 if root/scale/chord changed since the last publish (or first
        // block); then publish at each chord-write frame that changes the value.
        let cur = self.current(io);
        if self.last != Some(cur) {
            io.publish_context(CTX_OUT, 0, cur);
            self.last = Some(cur);
        }
        for (frame, chord) in writes {
            self.chord = chord;
            let cur = self.current(io);
            if self.last != Some(cur) {
                io.publish_context(CTX_OUT, frame, cur);
                self.last = Some(cur);
            }
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(ContextOp);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::ChordTag;
    use crate::message::{Event, Message};
    use crate::operator::CtxPublish;

    const SR: f32 = 48_000.0;

    fn default_params() -> Vec<f32> {
        ContextOp::descriptor().default_params()
    }

    /// Run one block; return published snapshots (block-absolute frames).
    fn run(op: &mut ContextOp, n: usize, params: &[f32], events: &[Message]) -> Vec<CtxPublish> {
        let evs: Vec<Event> = events
            .iter()
            .map(|m| Event {
                addr: &m.addr,
                args: &m.args,
                frame: m.frame,
            })
            .collect();
        let mut pubs: Vec<CtxPublish> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let mut io =
                Io::new(SR, n, inputs, outs, params, &evs).with_context_publish(&mut pubs, 0);
            op.process(&mut io);
        }
        pubs
    }

    #[test]
    fn publishes_default_once_then_stays_quiet() {
        let mut op = ContextOp::new();
        let p = default_params();
        let first = run(&mut op, 128, &p, &[]);
        assert_eq!(first.len(), 1, "first block publishes the initial context");
        assert_eq!(first[0].frame, 0);
        assert_eq!(first[0].ctx, Context::default());
        // No change → no further publishes.
        let second = run(&mut op, 128, &p, &[]);
        assert!(second.is_empty(), "unchanged context does not re-publish");
    }

    #[test]
    fn chord_write_publishes_at_its_frame() {
        let mut op = ContextOp::new();
        let p = default_params();
        let _ = run(&mut op, 128, &p, &[]); // consume the initial publish
        let set = Message::new(
            "chord",
            [
                Arg::Float(1.0),
                Arg::Float(0.0),
                Arg::Float(2.0),
                Arg::Float(4.0),
            ],
            40,
        );
        let pubs = run(&mut op, 128, &p, &[set]);
        assert_eq!(pubs.len(), 1);
        assert_eq!(pubs[0].frame, 40);
        assert_eq!(pubs[0].ctx.chord.tag, ChordTag::ScaleRelative);
    }

    #[test]
    fn root_change_publishes() {
        let mut op = ContextOp::new();
        let mut p = default_params();
        let _ = run(&mut op, 128, &p, &[]);
        p[P_ROOT] = 62.0; // move to D
        let pubs = run(&mut op, 128, &p, &[]);
        assert_eq!(pubs.len(), 1);
        assert_eq!(pubs[0].ctx.root, 62);
    }
}
