//! `integrate` — discrete running sum, `out[i] = Σ in[0..=i]`, per sample (ADR-0029).
//!
//! A dense `Float`→`Float` op with a **constant one-sample `dt`**: the running Riemann sum of the
//! input, accumulated across block boundaries. The inverse of [`differentiate`](super::differentiate)
//! — integrate a constant and you get a linear ramp; integrate a ramp and you get a parabola. As
//! with `differentiate`, the sampling window is literally one sample (no `sr` scaling); conversion
//! to a real time base ("·seconds") is a **separate, deferred** op, so the accumulator grows by the
//! raw sample value each step.
//!
//! The accumulator carries across blocks; `spawn` resets it to 0.
//!
//! - input 0: `in` (`Float`) — the signal to integrate. Unwired default 0.
//! - output 0: `out` (`Float`) — the running sum including the current sample.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0028/0029): `in` is a materialized `Float` (default 0).
crate::operator_contract!(Integrate {
    inputs:  { in: float { -1_000_000.0..=1_000_000.0, default 0.0, "", lin } },
    outputs: { out: float },
});

/// The op's scalar math, written once (ADR-0029 pure-fn seam): one accumulation step.
#[inline]
fn accumulate(acc: f32, cur: f32) -> f32 {
    acc + cur
}

#[derive(Default)]
pub struct Integrate {
    /// Running total, carried across block boundaries. Reset to 0 on `spawn`.
    acc: f32,
}

impl Integrate {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Integrate {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let mut acc = self.acc;
        for i in 0..n {
            let cur = io.signal(IN_IN).get(i).copied().unwrap_or(0.0);
            acc = accumulate(acc, cur);
            io.output(OUT_OUT)[i] = acc;
        }
        self.acc = acc;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Integrate);

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

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
    fn constant_input_ramps_linearly() {
        // Integrating a constant 2 yields a linear ramp 2, 4, 6, 8 (the inverse of differentiate).
        let out = run(&mut Integrate::new(), &[2.0, 2.0, 2.0, 2.0]);
        assert_eq!(out, vec![2.0, 4.0, 6.0, 8.0]);
    }

    #[test]
    fn zero_input_stays_zero() {
        let out = run(&mut Integrate::new(), &[0.0, 0.0, 0.0]);
        assert_eq!(out, vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn accumulator_carries_across_blocks() {
        let mut i = Integrate::new();
        let _ = run(&mut i, &[1.0, 1.0, 1.0]); // ends at 3
        let out = run(&mut i, &[1.0, 1.0, 1.0]);
        assert_eq!(out, vec![4.0, 5.0, 6.0]);
    }

    #[test]
    fn round_trips_with_differentiate() {
        // differentiate(integrate(x)) recovers x after the first (seeded) sample. Here x is a ramp.
        let x = [1.0, 2.0, 3.0, 4.0];
        let integ = run(&mut Integrate::new(), &x); // [1, 3, 6, 10]
        let mut d = super::super::differentiate::Differentiate::new();
        let back = run(&mut d, &integ);
        // First sample is the seeded 0; the rest recover x[1..].
        assert_eq!(back, vec![0.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn spawned_copy_resets_accumulator() {
        let mut i = Integrate::new();
        let _ = run(&mut i, &[10.0, 10.0]); // ends at 20
        let mut i2 = i.spawn();
        let out = run(&mut *i2, &[1.0, 1.0]);
        assert_eq!(out, vec![1.0, 2.0], "spawn starts the accumulator fresh");
    }
}
