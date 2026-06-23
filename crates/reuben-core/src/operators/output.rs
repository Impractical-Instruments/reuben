//! Output — the master sink.
//!
//! Passes its input through to its output so the Render loop can tap it as a master
//! channel (ADR-0009). Mixing many sources / n-channel routing lands later; for the
//! "first sound" run it is a single-channel passthrough.
//!
//! - input 0: `audio` (Signal)
//! - output 0: `audio` (Signal) — copy of the input, tapped as master.

use crate::descriptor::{Descriptor, LaneRule, Port};
use crate::operator::{Io, Operator};

pub const IN_AUDIO: usize = 0;
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
            inputs: vec![Port::signal("audio")],
            outputs: vec![Port::signal("audio")],
            params: vec![],
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        // Copy input -> output one sample at a time so the input borrow ends before each
        // output write — passthrough with no allocation (realtime-safe).
        for i in 0..n {
            let v = io.input(IN_AUDIO).map_or(0.0, |s| s[i]);
            io.output(OUT_AUDIO)[i] = v;
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Output);
