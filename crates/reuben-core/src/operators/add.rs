//! `add` — `out = a + b`, per sample (ADR-0029, ADR-0017).
//!
//! The sanctioned way to **combine two streams additively**: base-plus-modulation is an explicit
//! `add`. A dense `Float`→`Float` op — both operands are materialized `Float` inputs whose unwired
//! default is the additive identity `0`, so wiring only one side passes it through unchanged
//! (ADR-0028 materialize fills the other with zeros). One of the math family's pointwise members;
//! its arithmetic is the module-level [`add`] fn, called by the dense buffer shell in `process`
//! (the scalar-fn + shell seam that lets a future sparse/`Note`-field carrier reuse the same math
//! — issue #83).
//!
//! - input 0: `a` (`Float`) — first operand. Unwired default `0`.
//! - input 1: `b` (`Float`) — second operand. Unwired default `0`.
//! - output 0: `out` (`Float`) — `a + b`.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0028): one declaration -> IN_/OUT_ consts + Descriptor. Both
// operands are materialized `Float`s defaulting to the additive identity `0` (ADR-0029).
crate::operator_contract!(Add {
    inputs:  { a: float { -1_000_000.0..=1_000_000.0, default 0.0, "", lin },
               b: float { -1_000_000.0..=1_000_000.0, default 0.0, "", lin } },
    outputs: { out: float },
});

/// The op's scalar math, written once (ADR-0029 pure-fn seam). A future sparse/`Note`-field shell
/// (issue #83) reuses this rather than re-deriving the arithmetic.
#[inline]
fn add(a: f32, b: f32) -> f32 {
    a + b
}

#[derive(Default)]
pub struct Add;

impl Add {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for Add {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        for i in 0..n {
            // Each operand is a materialized `Float` (always a buffer in production); the
            // `unwrap_or` supplies the additive identity for the empty-slice (unwired) case, which
            // is also the declared default — so the descriptor and the loop never disagree.
            let a = io.signal(IN_A).get(i).copied().unwrap_or(0.0);
            let b = io.signal(IN_B).get(i).copied().unwrap_or(0.0);
            io.output(OUT_OUT)[i] = add(a, b);
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Add);

#[cfg(test)]
mod tests {
    use super::*;

    const SR: f32 = 48_000.0;

    /// Run `add` over one block with the given a/b inputs; returns `out`. A `None` operand stands
    /// in for the unwired case (the engine would materialize a zero buffer; the loop's `unwrap_or`
    /// reproduces it).
    fn run(a: Option<&[f32]>, b: Option<&[f32]>, n: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![a, b];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, n, inputs, outs, &[], &[]);
            Add::new().process(&mut io);
        }
        out
    }

    #[test]
    fn sums_two_buffers() {
        let a = [1.0, 2.0, 3.0];
        let b = [10.0, 20.0, 30.0];
        assert_eq!(run(Some(&a), Some(&b), 3), vec![11.0, 22.0, 33.0]);
    }

    #[test]
    fn unwired_b_passes_a_through() {
        // Additive identity 0: wiring only `a` leaves it unchanged (base-plus-modulation).
        let a = [5.0, 6.0, 7.0];
        assert_eq!(run(Some(&a), None, 3), vec![5.0, 6.0, 7.0]);
    }

    #[test]
    fn operand_defaults_are_the_additive_identity() {
        // The unwired default of both operands is 0 (data, not code) — the property that makes
        // "wire one side ⇒ passthrough" fall out of materialize (ADR-0029).
        let d = Add::descriptor();
        for name in ["a", "b"] {
            let (_, meta) = d
                .settable_inputs()
                .find(|(n, _)| *n == name)
                .expect("operand is a settable Float");
            assert_eq!(meta.default, 0.0, "{name} default");
        }
    }
}
