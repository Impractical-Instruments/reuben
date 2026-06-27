//! `mul` — `out = a * b`, per sample (ADR-0029, ADR-0017).
//!
//! The sanctioned way to **combine two streams multiplicatively**: ring-modulation, amplitude
//! scaling, and the VCA (`env.cv -> power -> mul`, audio on the other input — ADR-0027). A dense
//! `Float`→`Float` op — both operands are materialized `Float` inputs whose unwired default is the
//! multiplicative identity `1`, so wiring only one side passes it through unchanged (ADR-0028
//! materialize fills the other with ones). Its arithmetic is the module-level [`mul`] fn, called by
//! the dense buffer shell in `process` (the scalar-fn + shell seam — issue #83).
//!
//! - input 0: `a` (`Float`) — first operand. Unwired default `1`.
//! - input 1: `b` (`Float`) — second operand. Unwired default `1`.
//! - output 0: `out` (`Buffer`) — `a * b`.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0030). Both operands are materialized `Float`s defaulting to the
// multiplicative identity `1` (ADR-0029).
crate::operator_contract!(MulF32Signal {
    inputs:  { a: f32_buffer { -1_000_000.0..=1_000_000.0, default 1.0, "", lin },
               b: f32_buffer { -1_000_000.0..=1_000_000.0, default 1.0, "", lin } },
    outputs: { out: f32_buffer },
});

/// The op's scalar math, written once (ADR-0029 pure-fn seam).
#[inline]
fn mul(a: f32, b: f32) -> f32 {
    a * b
}

#[derive(Default)]
pub struct MulF32Signal;

impl MulF32Signal {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for MulF32Signal {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        for i in 0..n {
            // Materialized `Float` operands; `unwrap_or` supplies the multiplicative identity for
            // the empty-slice (unwired) case, matching the declared default.
            let a = io.input::<&[f32]>(IN_A).get(i).copied().unwrap_or(1.0);
            let b = io.input::<&[f32]>(IN_B).get(i).copied().unwrap_or(1.0);
            io.output::<&mut [f32]>(OUT_OUT)[i] = mul(a, b);
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(MulF32Signal);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Drive `mul` through the real engine; a `None` operand stands in for unwired (the engine
    /// materializes its multiplicative-identity default `1`), a `Some(buf)` drives a buffer.
    fn run(a: Option<&[f32]>, b: Option<&[f32]>, n: usize) -> Vec<f32> {
        let mut d = OpDriver::for_type(MulF32Signal::new(), SR);
        if let Some(a) = a {
            d.drive(IN_A, a);
        }
        if let Some(b) = b {
            d.drive(IN_B, b);
        }
        d.render(n).output(OUT_OUT).to_vec()
    }

    #[test]
    fn products_two_buffers() {
        let a = [1.0, 2.0, 4.0];
        let b = [3.0, 3.0, 0.5];
        assert_eq!(run(Some(&a), Some(&b), 3), vec![3.0, 6.0, 2.0]);
    }

    #[test]
    fn unwired_b_is_unity() {
        // Multiplicative identity 1: wiring only `a` passes it through.
        let a = [5.0, 6.0, 7.0];
        assert_eq!(run(Some(&a), None, 3), vec![5.0, 6.0, 7.0]);
    }

    #[test]
    fn operand_defaults_are_the_multiplicative_identity() {
        let d = MulF32Signal::descriptor();
        for name in ["a", "b"] {
            let (_, meta) = d
                .settable_inputs()
                .find(|(n, _)| *n == name)
                .expect("operand is a settable Float");
            assert_eq!(meta.default, 1.0, "{name} default");
        }
    }
}
