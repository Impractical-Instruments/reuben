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
//! - output 0: `out` (`Buffer`) — the running sum including the current sample.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0029/0030): `in` is a materialized `Float` (default 0).
crate::operator_contract!(IntegrateF32Signal {
    inputs:  { in: f32_buffer { -1_000_000.0..=1_000_000.0, default 0.0, "", lin } },
    outputs: { out: f32_buffer },
});

/// The op's scalar math, written once (ADR-0029 pure-fn seam): one accumulation step.
#[inline]
fn accumulate(acc: f32, cur: f32) -> f32 {
    acc + cur
}

#[derive(Default)]
pub struct IntegrateF32Signal {
    /// Running total, carried across block boundaries. Reset to 0 on `spawn`.
    acc: f32,
}

impl IntegrateF32Signal {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for IntegrateF32Signal {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let mut acc = self.acc;
        for i in 0..n {
            let cur = io.input::<&[f32]>(IN_IN).get(i).copied().unwrap_or(0.0);
            acc = accumulate(acc, cur);
            io.output::<&mut [f32]>(OUT_OUT)[i] = acc;
        }
        self.acc = acc;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(IntegrateF32Signal);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::{OpDriver, BLOCK_SIZE};
    use crate::operators::differentiate::DifferentiateF32Signal;

    const SR: f32 = 48_000.0;

    /// IntegrateF32Signal `input` through the real engine (one driver, block-sliced). `in` is the per-sample
    /// `Float` buffer (`drive`d); `out` is read back.
    fn run(input: &[f32]) -> Vec<f32> {
        OpDriver::for_type(IntegrateF32Signal::new(), SR)
            .drive(IN_IN, input)
            .render(input.len())
            .output(OUT_OUT)
            .to_vec()
    }

    #[test]
    fn constant_input_ramps_linearly() {
        // Integrating a constant 2 yields a linear ramp 2, 4, 6, 8 (the inverse of differentiate).
        assert_eq!(run(&[2.0, 2.0, 2.0, 2.0]), vec![2.0, 4.0, 6.0, 8.0]);
    }

    #[test]
    fn zero_input_stays_zero() {
        assert_eq!(run(&[0.0, 0.0, 0.0]), vec![0.0, 0.0, 0.0]);
    }

    #[test]
    fn accumulator_carries_across_blocks() {
        // A constant 1 spanning several real 128-frame blocks integrates to the running ramp
        // 1, 2, 3, …, n. If the accumulator reset at each block boundary the sum would drop back to
        // 1 at frames 128, 256, … — the strictly-rising ramp proves it carries across them.
        let n = 3 * BLOCK_SIZE;
        let out = OpDriver::for_type(IntegrateF32Signal::new(), SR)
            .set(IN_IN, 1.0)
            .render(n)
            .output(OUT_OUT)
            .to_vec();
        for (i, &s) in out.iter().enumerate() {
            assert_eq!(s, (i + 1) as f32, "running sum at frame {i}");
        }
    }

    #[test]
    fn round_trips_with_differentiate() {
        // differentiate(integrate(x)) recovers x after the first (seeded) sample. Here x is a ramp.
        let x = [1.0, 2.0, 3.0, 4.0];
        let integ = run(&x); // [1, 3, 6, 10]
        let back = OpDriver::for_type(DifferentiateF32Signal::new(), SR)
            .drive(IN_IN, &integ)
            .render(integ.len())
            .output(OUT_OUT)
            .to_vec();
        // First sample is the seeded 0; the rest recover x[1..].
        assert_eq!(back, vec![0.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn spawned_copy_resets_accumulator() {
        let mut base = OpDriver::for_type(IntegrateF32Signal::new(), SR);
        base.drive(IN_IN, &[10.0, 10.0]).render(2); // ends at 20
        let out = base
            .spawn()
            .drive(IN_IN, &[1.0, 1.0])
            .render(2)
            .output(OUT_OUT)
            .to_vec();
        assert_eq!(out, vec![1.0, 2.0], "spawn starts the accumulator fresh");
    }
}
