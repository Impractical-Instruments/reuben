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

/// Value-carrier form of `mul` (ADR-0031): all-`f32` **held** operands, one held output. Reads each
/// operand's held (ZOH) value once and emits the product as a single deduped `MsgWriter` change.
/// Reuses the shared scalar [`mul`] (issue #83 seam) — the value shell calls it once where the
/// signal shell loops it. Block-slicing re-runs `process` at every operand change (post-flip, when
/// the ports are Value), so the sparse output is sample-accurate with no buffer. A forced submodule:
/// the contract macro emits its `IN_`/`OUT_` consts at module scope, so the two forms can't share one.
pub mod value {
    use super::mul;
    use crate::descriptor::Descriptor;
    use crate::operator::{Io, Operator};

    // Same operands as the signal form, multiplicative identity `1` — but `f32` (held), not buffers.
    crate::operator_contract!(MulF32Value {
        inputs:  { a: f32 { -1_000_000.0..=1_000_000.0, default 1.0, "", lin },
                   b: f32 { -1_000_000.0..=1_000_000.0, default 1.0, "", lin } },
        outputs: { out: f32 { -1_000_000.0..=1_000_000.0, default 0.0, "", lin } },
    });

    #[derive(Default)]
    pub struct MulF32Value;

    impl MulF32Value {
        pub fn new() -> Self {
            Self
        }
    }

    impl Operator for MulF32Value {
        fn descriptor() -> Descriptor {
            Self::contract()
        }

        fn process(&mut self, io: &mut Io) {
            // Held operands, read once; `unwrap_or` supplies the multiplicative identity for the
            // unwired case, matching the declared default.
            let a = io.input::<f32>(IN_A).unwrap_or(1.0);
            let b = io.input::<f32>(IN_B).unwrap_or(1.0);
            io.output::<f32>(OUT_OUT).set(0, mul(a, b));
        }

        fn spawn(&self) -> Box<dyn Operator> {
            Box::new(Self::new())
        }
    }

    crate::register_operator!(MulF32Value);

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::message::{Arg, Emit};
        use crate::op_driver::OpDriver;

        const SR: f32 = 48_000.0;

        /// The F32 value carried by an emit (panics on any other Arg — the contract is F32).
        fn val(e: &Emit) -> f32 {
            match &e.arg {
                Arg::F32(v) => *v,
                other => panic!("expected an F32 product, got {other:?}"),
            }
        }

        /// Drive the held operands as block-rate constants; returns the emitted product value(s).
        /// A `None` operand is left unwired (engine seeds its multiplicative-identity default `1`).
        fn run(a: Option<f32>, b: Option<f32>) -> Vec<f32> {
            let mut d = OpDriver::for_type(MulF32Value::new(), SR);
            if let Some(a) = a {
                d.set(IN_A, a);
            }
            if let Some(b) = b {
                d.set(IN_B, b);
            }
            d.render(64).emits().iter().map(val).collect()
        }

        #[test]
        fn products_held_operands() {
            assert_eq!(run(Some(4.0), Some(2.5)), vec![10.0]);
        }

        #[test]
        fn unwired_b_is_unity() {
            // Multiplicative identity 1: wiring only `a` emits it unchanged.
            assert_eq!(run(Some(7.0), None), vec![7.0]);
        }

        #[test]
        fn operand_defaults_are_the_multiplicative_identity() {
            let d = MulF32Value::descriptor();
            for name in ["a", "b"] {
                let (_, meta) = d
                    .settable_inputs()
                    .find(|(n, _)| *n == name)
                    .expect("operand is a settable Float");
                assert_eq!(meta.default, 1.0, "{name} default");
            }
        }
    }
}

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
