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
//! - output 0: `out` (`Float`) — `in[i] - in[i-1]`.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0028/0029): `in` is a materialized `Float` (default 0).
crate::operator_contract!(Differentiate {
    inputs:  { in: float { -1_000_000.0..=1_000_000.0, default 0.0, "", lin } },
    outputs: { out: float },
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
            let cur = io.signal(IN_IN).get(i).copied().unwrap_or(0.0);
            // First sample ever seeds `last = cur`, so it emits 0 (no predecessor ⇒ no change).
            let prev = last.unwrap_or(cur);
            io.output(OUT_OUT)[i] = step(prev, cur);
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

    const SR: f32 = 48_000.0;

    /// Run `differentiate` over one block; returns `out`. Reuses `op` across calls so block-boundary
    /// state can be exercised.
    fn run(op: &mut dyn Operator, input: &[f32]) -> Vec<f32> {
        let n = input.len();
        let mut out = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(input)];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, n, inputs, outs, &[], &[]);
            op.process(&mut io);
        }
        out
    }

    #[test]
    fn constant_input_has_zero_derivative() {
        let out = run(&mut Differentiate::new(), &[3.0, 3.0, 3.0, 3.0]);
        assert_eq!(out, vec![0.0, 0.0, 0.0, 0.0]);
    }

    #[test]
    fn ramp_has_constant_unit_steps() {
        // A unit-per-sample ramp differentiates to a constant 1, after the seeded first sample (0).
        let out = run(&mut Differentiate::new(), &[0.0, 1.0, 2.0, 3.0]);
        assert_eq!(out, vec![0.0, 1.0, 1.0, 1.0]);
    }

    #[test]
    fn first_sample_never_spikes() {
        // No predecessor for the first ever sample, whatever its value — seeded, emits 0.
        let out = run(&mut Differentiate::new(), &[5.0, 6.0, 7.0]);
        assert_eq!(out, vec![0.0, 1.0, 1.0]);
    }

    #[test]
    fn predecessor_carries_across_blocks() {
        // Block 2's first sample differences against block 1's last sample (2.0), not a re-seed.
        let mut d = Differentiate::new();
        let _ = run(&mut d, &[0.0, 1.0, 2.0]);
        let out = run(&mut d, &[3.0, 4.0, 5.0]);
        assert_eq!(out, vec![1.0, 1.0, 1.0]);
    }

    #[test]
    fn spawned_copy_re_seeds() {
        let mut d = Differentiate::new();
        let _ = run(&mut d, &[0.0, 10.0, 20.0]);
        let mut d2 = d.spawn();
        // Fresh: first sample after spawn seeds (emits 0), exactly like a new op.
        let out = run(&mut *d2, &[100.0, 101.0]);
        assert_eq!(out, vec![0.0, 1.0]);
    }
}
