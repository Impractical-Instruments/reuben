//! Context — the tonal-context node: broadcasts the current [`Harmony`] (key/scale/chord) (ADR-0013).
//!
//! It owns the latched [`Harmony`] and publishes it onto a Harmony output port; followers
//! (the Voicer's degree resolution, a snap op) read "what's the key/chord right now" via
//! `io.harmony()`. A single default instance in a Rig makes everything agree out of the box
//! — the same on-ramp as the default Clock — without baking *global* into the core
//! (multiple context nodes = polytonality).
//!
//! Per-field **last-write-wins** (ADR-0013):
//! - **Static fields** — `root` and the scale (`degrees` + `s0`..`s11` step offsets) — are
//!   `Float` inputs (ADR-0028; the good-button: dial the key, shape the scale). They are per-sample
//!   buffers, so `process` scans them for change frames and publishes at the exact frame — a
//!   mid-block `/context/root` stays sample-accurate (ADR-0015) and each can now be
//!   *wired*/modulated. (Named scales like `"dorian"` and non-12-TET tunings are a deferred
//!   preset/registry layer; the scale here is the explicit step-offset list ADR-0013 specifies.)
//! - **Dynamic field** — `chord` — arrives as a `chord` Message on the `set` input
//!   (`/context/chord [tag, d0, d1, …]`, `tag`: 0 = clear, 1 = scale-relative, 2 = absolute).
//!
//! The node publishes **on change** (emit-on-change, ADR-0015): the first block, and any
//! block where root/scale/chord differ from the last published value — so steady state is
//! allocation-free. A chord change mid-block publishes at the change frame, so it is
//! sample-accurate on the same timeline as notes.
//!
//! - input `set` (Message) — `chord` writes (also reachable from an internal chord-progression
//!   op wired to this port; that op is deferred).
//! - inputs `root`, `degrees`, `s0`..`s11` (`Float`) — the static key/scale fields.
//! - output `ctx` (Harmony) — the latched context followers read.

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::harmony::{Chord, ChordTag, Harmony, ScaleField, CHORD_CAP, SCALE_CAP};
use crate::message::Arg;
use crate::operator::{Io, Operator};

/// Number of scale step-offset slots (max scale length within a 12-TET period).
pub const NUM_STEPS: usize = SCALE_CAP;

// Single-source contract (ADR-0025/0028): one declaration -> IN_/OUT_ consts + Descriptor.
// `root`, `degrees`, and `s0`..`s11` are now `Float` inputs (read block-rate via `io.value`),
// so they can be wired/modulated; `set` stays a Message; `ctx` is the Harmony carrier output.
crate::operator_contract!(ContextOp {
    type_name: "context",
    inputs:  { set:     message,
               root:    float { 0.0..=127.0,      default 60.0,  "MIDI",  lin },
               degrees: float { 1.0..=12.0,       default 7.0,   "steps", lin },
               s0:      float { -24.0..=24.0,     default 0.0,   "steps", lin },
               s1:      float { -24.0..=24.0,     default 2.0,   "steps", lin },
               s2:      float { -24.0..=24.0,     default 4.0,   "steps", lin },
               s3:      float { -24.0..=24.0,     default 5.0,   "steps", lin },
               s4:      float { -24.0..=24.0,     default 7.0,   "steps", lin },
               s5:      float { -24.0..=24.0,     default 9.0,   "steps", lin },
               s6:      float { -24.0..=24.0,     default 11.0,  "steps", lin },
               s7:      float { -24.0..=24.0,     default 0.0,   "steps", lin },
               s8:      float { -24.0..=24.0,     default 0.0,   "steps", lin },
               s9:      float { -24.0..=24.0,     default 0.0,   "steps", lin },
               s10:     float { -24.0..=24.0,     default 0.0,   "steps", lin },
               s11:     float { -24.0..=24.0,     default 0.0,   "steps", lin } },
    outputs: { ctx: context },
});

/// Input ordinal of the first scale step offset; degree `k` is input `IN_S0 + k`.
pub const IN_STEP0: usize = IN_S0;

pub struct ContextOp {
    /// Latched chord, persisted across blocks (LWW from `chord` writes).
    chord: Chord,
    /// Last value published, to publish only on change (ADR-0015). `None` until the first
    /// block, which always publishes (so the baseline picks up a non-default config).
    last: Option<Harmony>,
    /// Reused scratch for the candidate publish frames, cleared each block and kept across
    /// blocks so steady state never reallocates (mirrors the render loop's reused `bounds`).
    /// A `Float` field wired to a per-sample source can push up to `block_size - 1` change
    /// frames; a fixed inline buffer would spill to the heap on the audio thread, so this is a
    /// node-owned `Vec` that grows once to block size during warmup.
    frames: Vec<usize>,
}

impl Default for ContextOp {
    fn default() -> Self {
        Self {
            chord: Chord::empty(),
            last: None,
            frames: Vec::new(),
        }
    }
}

impl ContextOp {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the current context from the `Float` inputs **at frame `f`** + the latched chord.
    /// The static fields are per-sample buffers now (ADR-0028), so reading them at the change
    /// frame is what keeps a mid-block `/context/root` sample-accurate (ADR-0015).
    fn current_at(&self, io: &Io, f: usize) -> Harmony {
        let at = |port| {
            io.signal(port)
                .get(f)
                .copied()
                .unwrap_or_else(|| io.value(port))
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

impl Operator for ContextOp {
    fn descriptor() -> Descriptor {
        // Default scale = C major; root = middle C (the per-input defaults in the contract). So
        // the default context equals the engine default (existing rigs unchanged).
        Self::contract()
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

        // Candidate publish frames: frame 0 (first-block / cross-block change), every interior
        // frame where a static `Float` field changes (scan only the inputs that varied this block,
        // so steady state is free — ADR-0028), and every chord-write frame. A static field is a
        // per-sample buffer now, so a mid-block `/context/root` lands at its exact frame.
        let n = io.frames();
        // Take the reused scratch out so the publish walk below can borrow `self` mutably; it
        // is restored at the end, retaining its capacity for the next block (alloc-free steady
        // state). `mem::take` leaves an empty `Vec` behind, which does not allocate.
        let mut frames = std::mem::take(&mut self.frames);
        frames.clear();
        frames.push(0);
        for port in [IN_ROOT, IN_DEGREES]
            .into_iter()
            .chain(IN_STEP0..IN_STEP0 + NUM_STEPS)
        {
            if io.varying(port) {
                let buf = io.signal(port);
                for i in 1..buf.len().min(n) {
                    if buf[i] != buf[i - 1] {
                        frames.push(i);
                    }
                }
            }
        }
        for (f, _) in &writes {
            frames.push(*f);
        }
        frames.sort_unstable();
        frames.dedup();

        // Walk the frames in order: apply any chord write landing here (LWW), then publish the
        // resolved context if it differs from the last published value.
        for &f in &frames {
            for (wf, chord) in &writes {
                if *wf == f {
                    self.chord = *chord;
                }
            }
            let cur = self.current_at(io, f);
            if self.last != Some(cur) {
                io.publish_harmony(OUT_CTX, f, cur);
                self.last = Some(cur);
            }
        }

        self.frames = frames;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(ContextOp);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harmony::ChordTag;
    use crate::message::{Event, Message};
    use crate::operator::CtxPublish;

    const SR: f32 = 48_000.0;

    /// Number of Float inputs (root, degrees, s0..s11) — the `set` Message input excluded.
    const NUM_VALUES: usize = NUM_STEPS + 2;

    /// Default values for the Float inputs in port order (root, degrees, s0..s11), pulled from
    /// the contract's per-input defaults. Index with `IN_ROOT - 1` etc. (offset by the leading
    /// `set` Message input).
    fn default_values() -> Vec<f32> {
        ContextOp::descriptor()
            .inputs
            .iter()
            .filter_map(|p| p.meta.as_ref().map(|m| m.default))
            .collect()
    }

    /// Run one block; return published snapshots (block-absolute frames). `values` are the Float
    /// inputs (root, degrees, s0..s11) in port order — each materialized into a constant per-sample
    /// buffer the way the engine would for an unwired input. `set` is delivered via `events`.
    fn run(op: &mut ContextOp, n: usize, values: &[f32], events: &[Message]) -> Vec<CtxPublish> {
        let evs: Vec<Event> = events
            .iter()
            .map(|m| Event {
                addr: &m.addr,
                args: &m.args,
                frame: m.frame,
            })
            .collect();
        let bufs: Vec<Vec<f32>> = values.iter().map(|&v| vec![v; n]).collect();
        let mut pubs: Vec<CtxPublish> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            // Port order: set (Message, no buffer), then root, degrees, s0..s11 (Float buffers).
            let mut inputs: Vec<Option<&[f32]>> = Vec::with_capacity(NUM_VALUES + 1);
            inputs.push(None);
            inputs.extend(bufs.iter().map(|b| Some(&b[..])));
            let params: [f32; 0] = [];
            let mut io =
                Io::new(SR, n, inputs, outs, &params, &evs).with_context_publish(&mut pubs, 0);
            op.process(&mut io);
        }
        pubs
    }

    #[test]
    fn publishes_default_once_then_stays_quiet() {
        let mut op = ContextOp::new();
        let p = default_values();
        let first = run(&mut op, 128, &p, &[]);
        assert_eq!(first.len(), 1, "first block publishes the initial context");
        assert_eq!(first[0].frame, 0);
        assert_eq!(first[0].ctx, Harmony::default());
        // No change → no further publishes.
        let second = run(&mut op, 128, &p, &[]);
        assert!(second.is_empty(), "unchanged context does not re-publish");
    }

    #[test]
    fn chord_write_publishes_at_its_frame() {
        let mut op = ContextOp::new();
        let p = default_values();
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
        let mut p = default_values();
        let _ = run(&mut op, 128, &p, &[]);
        p[IN_ROOT - 1] = 62.0; // move to D ( `values` excludes the leading `set` Message input )
        let pubs = run(&mut op, 128, &p, &[]);
        assert_eq!(pubs.len(), 1);
        assert_eq!(pubs[0].ctx.root, 62);
    }
}
