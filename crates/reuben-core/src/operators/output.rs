//! Output — the master sink.
//!
//! Passes its input through to its output so the Render loop can tap it as a master
//! channel (ADR-0009). Mixing many sources / n-channel routing lands later; for the
//! "first sound" run it is a single-channel passthrough.
//!
//! - input 0: `audio` (`Buffer`) — per-sample audio in (the wired master bus).
//! - output 0: `audio` (`Buffer`) — copy of the input, tapped as master.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0030): one declaration -> typed IN_/OUT_ handles + the
// Descriptor. Was the one hand-written descriptor; folded into the macro with the typed-handle
// switch (ADR-0037) so its ports get handles like every other operator.
crate::operator_contract!(Output {
    inputs:  { audio: f32_buffer },
    outputs: { audio: f32_buffer },
});

#[derive(Default)]
pub struct Output;

impl Output {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for Output {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        // Unity passthrough. The input slice borrows the arena (not `io`), so it stays valid
        // alongside the mutable output borrow — sample-exact copy, no allocation (realtime-safe).
        let input = io.read(IN_AUDIO);
        io.write(OUT_AUDIO)[..n].copy_from_slice(&input[..n]);
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Output);
