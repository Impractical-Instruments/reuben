//! Output — the master sink.
//!
//! Passes its input through to its output so the Render loop can tap it as a master
//! channel (ADR-0009). Mixing many sources / n-channel routing lands later; for the
//! "first sound" run it is a single-channel passthrough.
//!
//! - input 0: `audio` (`Float`) — per-sample buffer (wired source or materialized latch).
//! - output 0: `audio` (`Float`) — copy of the input, tapped as master.

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::operator::{Io, Operator};

/// `audio` input (`Float`).
pub const IN_AUDIO: usize = 0;
/// `audio` output (`Float`).
pub const OUT_AUDIO: usize = 0;

#[derive(Default)]
pub struct Output;

impl Output {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for Output {
    fn descriptor() -> Descriptor {
        Descriptor {
            type_name: "output",
            inputs: vec![Port::float(ParamMeta {
                name: "audio",
                min: -1.0,
                max: 1.0,
                default: 0.0,
                unit: "",
                curve: Curve::Linear,
            })],
            outputs: vec![Port::signal("audio")],
            params: vec![],
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        // Copy input -> output one sample at a time so the input borrow ends before each
        // output write — passthrough with no allocation (realtime-safe). `audio` is a `Float`
        // input, so it is always a buffer (wired source or materialized latch) — one read path.
        for i in 0..n {
            let v = io.signal(IN_AUDIO).get(i).copied().unwrap_or(0.0);
            io.output(OUT_AUDIO)[i] = v;
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Output);
