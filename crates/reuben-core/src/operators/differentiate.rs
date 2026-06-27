//! `differentiate` — discrete rate of change, `out[i] = in[i] - in[i-1]`, per sample (ADR-0029).
//!
//! A dense `Float`→`Float` op with a **constant one-sample `dt`**: the change between adjacent
//! samples. A constant sampling window is what makes higher-order calculus valid — differentiate a
//! signal twice and you get acceleration, which is only meaningful when `dt` does not vary (an
//! irregular sparse Δt cannot guarantee that). Conversion to a real time base ("change per second",
//! "per beat") is a **separate, deferred** op — `dt` is literally one sample here.
//!
//! Gesture velocity (the prior Message-domain behavior) is recovered by materializing the gesture
//! into a dense CV first (`m2s`/slew) and then differentiating it.
//!
//! The very first sample of an instance has no predecessor, so it seeds `last = in[0]` and emits
//! `0` — no startup spike. The predecessor carries across block boundaries; `spawn` resets it.
//!
//! - input 0: `in` (`Float`) — the signal to differentiate. Unwired default 0.
//! - output 0: `out` (`Buffer`) — `in[i] - in[i-1]`.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0029/0030): `in` is a materialized `Float` (default 0).
crate::operator_contract!(Differentiate {
    inputs:  { in: f32 { -1_000_000.0..=1_000_000.0, default 0.0, "", lin } },
    outputs: { out: f32_buffer },
});

/// The op's scalar math, written once (ADR-0029 pure-fn seam): the one-sample difference.
#[inline]
fn step(prev: f32, cur: f32) -> f32 {
    cur - prev
}

#[derive(Default)]
pub struct Differentiate {
    /// Previous sample, carried across block boundaries. `None` until the first sample ever, which
    /// seeds it (so that sample emits 0 — no startup spike).
    last: Option<f32>,
}

impl Differentiate {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Differentiate {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let mut last = self.last;
        for i in 0..n {
            let cur = io.input::<&[f32]>(IN_IN).get(i).copied().unwrap_or(0.0);
            // First sample ever seeds `last = cur`, so it emits 0 (no predecessor ⇒ no change).
            let prev = last.unwrap_or(cur);
            io.output::<&mut [f32]>(OUT_OUT)[i] = step(prev, cur);
            last = Some(cur);
        }
        self.last = last;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Differentiate);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::{OpDriver, BLOCK_SIZE};

    const SR: f32 = 48_000.0;

    /// Differentiate `input` through the real engine (one driver, block-sliced). `in` is the
    /// per-sample `Float` buffer (`drive`d); `out` is read back.
    fn run(input: &[f32]) -> Vec<f32> {
        OpDriver::for_type(Differentiate::new(), SR)
            .drive(IN_IN, input)
            .render(input.len())
            .output(OUT_OUT)
            .to_vec()
    }

    #[test]
    fn constant_input_has_zero_derivative() {
        assert_eq!(run(&[3.0, 3.0, 3.0, 3.0]), vec![0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn ramp_has_constant_unit_steps() {
        // A unit-per-sample ramp differentiates to a constant 1, after the seeded first sample (0).
        assert_eq!(run(&[0.0, 1.0, 2.0, 3.0]), vec![0.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn first_sample_never_spikes() {
        // No predecessor for the first ever sample, whatever its value — seeded, emits 0.
        assert_eq!(run(&[5.0, 6.0, 7.0]), vec![0.0, 1.0, 1.0]);
    }

    #[test]
    fn predecessor_carries_across_blocks() {
        // A unit ramp spanning several real 128-frame blocks differentiates to a constant 1 after
        // the seeded first sample. If the predecessor re-seeded at each block boundary, the first
        // sample of blocks 2, 3, … would spike to 0 — so an all-ones tail proves it carries across
        // the engine's block slicing.
        let n = 3 * BLOCK_SIZE;
        let input: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let out = run(&input);
        assert_eq!(out[0], 0.0, "seeded first sample emits 0");
        assert!(
            out[1..].iter().all(|&s| s == 1.0),
            "every later sample (across block boundaries) is a unit step"
        );
    }

    #[test]
    fn spawned_copy_re_seeds() {
        let mut base = OpDriver::for_type(Differentiate::new(), SR);
        base.drive(IN_IN, &[0.0, 10.0, 20.0]).render(3);
        // Fresh: first sample after spawn seeds (emits 0), exactly like a new op.
        let out = base
            .spawn()
            .drive(IN_IN, &[100.0, 101.0])
            .render(2)
            .output(OUT_OUT)
            .to_vec();
        assert_eq!(out, vec![0.0, 1.0]);
    }
}
