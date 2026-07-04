//! `pipe` — an interface pipe's runtime node (ADR-0038 §2, format v2).
//!
//! An `interface.inputs` entry is a **named pipe**: it mints an address in the flat node
//! namespace (`in` → `/in`) and behaves like a source node — internal consumers wire from it
//! (`{"from": "/in"}`, fan-out free), and whatever feeds the boundary (a parent edge through the
//! synthesized face, a Voicer driving a voice, external OSC, or — P3 — the core input master)
//! lands on its single `in` port. This operator is that node: a pure single-port pass-through,
//! `in` → `out`, in the declared type's form.
//!
//! It is **loader-built, not registered**: a pipe exists only because an `interface.inputs`
//! entry declared it, its descriptor is synthesized per entry (the declared `Arg` type, range,
//! default), and it never appears in the operator registry, the schema's `type` enum, or
//! `describe`'s operator list. There is deliberately no `register_operator!` here.
//!
//! Per-form behavior (all allocation-free — hot path):
//!
//! - **Signal** (`f32_buffer`): copy the input buffer to the output buffer. Unwired, the input
//!   materializes from its latch — the declared default, or **silence** for a bare pipe — so an
//!   unfed pipe renders exactly what ADR-0038 promises.
//! - **Value** (`f32`, a vocab enum, `harmony`): forward the held latch as a sparse Value write.
//!   The dedup baseline is **operator state** (not the per-segment `MsgWriter`), so a pipe emits
//!   its seed once on the first block and then only on change — downstream latches see the same
//!   change frames a direct wire would have delivered.
//! - **Event** (`note`): re-emit every routed event at its frame, verbatim.

use crate::descriptor::{Descriptor, Port};
use crate::message::Arg;
use crate::operator::{EventWriter, Io, MsgWriter, Operator};
use crate::plan::PortKind;

/// The interface pipe (see module docs): a loader-built pass-through node whose descriptor is
/// synthesized from its `interface.inputs` entry. `kind` fixes which of the three forms
/// `process` forwards.
pub struct Pipe {
    kind: PortKind,
    /// Value pipes: the last forwarded Value, the **cross-block** dedup baseline (a fresh
    /// `MsgWriter`'s dedup is per-call). `None` until the first block, so the seed (declared
    /// default or author literal) is forwarded exactly once at frame 0.
    last: Option<Arg>,
}

impl Pipe {
    pub fn new(kind: PortKind) -> Self {
        Self { kind, last: None }
    }
}

impl Operator for Pipe {
    /// The generic placeholder contract (a bare signal pipe). The loader never reads this — it
    /// synthesizes each pipe's real descriptor from its `interface.inputs` entry and passes it
    /// to [`Graph::add_boxed`](crate::graph::Graph::add_boxed) directly.
    fn descriptor() -> Descriptor {
        Descriptor {
            type_name: "pipe",
            inputs: vec![Port::f32_buffer("in")],
            outputs: vec![Port::f32_buffer("out")],
            constants: Vec::new(),
            resources: Vec::new(),
        }
    }

    fn process(&mut self, io: &mut Io) {
        match self.kind {
            PortKind::Signal => {
                // Buffer-presence invariant (ADR-0037): the input is always a dense length-n
                // buffer (wired share, or materialized default/silence), so `copy_from_slice`
                // asserts the equal-length invariant instead of zip-truncating around a breach.
                let src = io.input::<&[f32]>(0);
                io.output::<&mut [f32]>(0).copy_from_slice(src);
            }
            PortKind::Value => {
                // Forward the held latch, type-erased: the pipe's declared type already
                // normalized it (an enum latches its concrete variant), so the raw Arg is
                // exactly what a downstream latch wants. Cheap clone — Value pipes carry
                // F32 / Enum / Harmony, none of which heap-allocate.
                let Some(arg) = io.latch_arg(0) else { return };
                if self.last.as_ref() != Some(arg) {
                    let arg = arg.clone();
                    MsgWriter::on(io, 0).set(0, arg.clone());
                    self.last = Some(arg);
                }
            }
            PortKind::Event => {
                let events = io.stream(0);
                let mut w = EventWriter::on(io, 0);
                for e in events {
                    w.emit(e.frame, e.arg.clone());
                }
            }
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new(self.kind))
    }
}

// Deliberately NOT `register_operator!`-ed: pipes are declared through `interface.inputs`
// entries only (ADR-0038); a document cannot name `"type": "pipe"` on a node.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Emit;

    #[test]
    fn signal_pipe_copies_its_input() {
        let mut pipe = Pipe::new(PortKind::Signal);
        let src = [0.5_f32, -0.25, 1.0];
        let mut out = [0.0_f32; 3];
        {
            let mut io = Io::new(48_000.0, 3, [Some(&src[..])], [&mut out[..]]);
            pipe.process(&mut io);
        }
        assert_eq!(out, src);
    }

    #[test]
    fn value_pipe_forwards_once_then_dedups_across_calls() {
        // One sink per call, like the engine (emit scratch is per-node-per-block).
        let mut pipe = Pipe::new(PortKind::Value);
        let run = |pipe: &mut Pipe, v: f32| -> Vec<Emit> {
            let mut sink: Vec<Emit> = Vec::new();
            let latch = [Arg::F32(v)];
            let mut io = Io::new(48_000.0, 8, [None], std::iter::empty::<&mut [f32]>())
                .with_latched(&latch)
                .with_emit(&mut sink, 0);
            pipe.process(&mut io);
            drop(io);
            sink
        };
        let first = run(&mut pipe, 440.0);
        assert_eq!(first.len(), 1, "the seed forwards once, at frame 0");
        assert_eq!((first[0].frame, &first[0].arg), (0, &Arg::F32(440.0)));
        assert!(
            run(&mut pipe, 440.0).is_empty(),
            "unchanged value emits nothing on later blocks"
        );
        let changed = run(&mut pipe, 220.0);
        assert_eq!(changed.len(), 1, "a changed latch forwards again");
        assert_eq!(changed[0].arg, Arg::F32(220.0));
    }

    #[test]
    fn event_pipe_reemits_every_event_at_its_frame() {
        use crate::message::Event;
        use crate::vocab::pitch::{Note, Pitch};
        let mut pipe = Pipe::new(PortKind::Event);
        let n0 = Arg::Note(Note::new(Pitch::from_midi(60.0), 1.0));
        let n1 = Arg::Note(Note::new(Pitch::from_midi(64.0), 0.5));
        let events = [Event { arg: &n0, frame: 3 }, Event { arg: &n1, frame: 7 }];
        let streams: [&[Event]; 1] = [&events];
        let mut sink: Vec<Emit> = Vec::new();
        {
            let mut io = Io::new(48_000.0, 16, [None], std::iter::empty::<&mut [f32]>())
                .with_streams(&streams)
                .with_emit(&mut sink, 0);
            pipe.process(&mut io);
        }
        let got: Vec<(usize, &Arg)> = sink.iter().map(|e| (e.frame, &e.arg)).collect();
        assert_eq!(got, vec![(3, &n0), (7, &n1)]);
    }

    #[test]
    fn spawn_resets_the_dedup_baseline() {
        // Voice copies spawn from the loader-built template (ADR-0032); a copy must forward
        // its first value even when the template already forwarded (and latched) the same one
        // — "spawn starts fresh" (authoring.md), asserted on the SPAWNED copy's behavior.
        let run = |pipe: &mut dyn Operator, v: f32| -> Vec<Emit> {
            let mut sink: Vec<Emit> = Vec::new();
            let latch = [Arg::F32(v)];
            let mut io = Io::new(48_000.0, 8, [None], std::iter::empty::<&mut [f32]>())
                .with_latched(&latch)
                .with_emit(&mut sink, 0);
            pipe.process(&mut io);
            drop(io);
            sink
        };
        let mut template = Pipe::new(PortKind::Value);
        assert_eq!(
            run(&mut template, 440.0).len(),
            1,
            "template forwards its seed"
        );
        assert!(
            run(&mut template, 440.0).is_empty(),
            "template deduped — its baseline is now 440"
        );
        let mut spawned = template.spawn();
        let first = run(spawned.as_mut(), 440.0);
        assert_eq!(
            first.len(),
            1,
            "the spawned copy forwards the same value again: its dedup baseline is reset"
        );
        assert_eq!((first[0].frame, &first[0].arg), (0, &Arg::F32(440.0)));
    }
}
