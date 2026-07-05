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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::{OpDriver, BLOCK_SIZE};

    #[test]
    fn output_is_a_sample_exact_unity_passthrough() {
        // The master sink's entire contract: copy exactly `n` input frames to the output,
        // bit-for-bit. The behavioral-equivalence pin in `op_driver.rs`
        // (`op_driver_output_matches_the_real_render_path_sample_exact`) treats the master tap
        // as the upstream node's raw output *because* `output` is a unity passthrough — this is
        // the test it cites. That pin only ever runs full 128-frame blocks, so drive a partial
        // final block too: the shape the engine produces whenever it block-slices at a change
        // frame, where an off-by-one in the `[..n]` copy would hide.
        let n = 2 * BLOCK_SIZE + 17; // partial final block: the copy holds per sub-block
        let samples: Vec<f32> = (0..n).map(|i| ((i * 7) % 31) as f32 / 31.0 - 0.5).collect();
        let mut d = OpDriver::for_type(Output::new(), 48_000.0);
        d.drive(IN_AUDIO, &samples);
        d.render(n);
        assert_eq!(
            d.output(OUT_AUDIO),
            &samples[..],
            "output must be a bit-exact unity passthrough"
        );
    }
}
