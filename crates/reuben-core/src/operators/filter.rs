//! Filter — state-variable filter, low-pass output.
//!
//! Ports/params are FROZEN (Stage A). DSP body is filled test-first in Stage B.
//!
//! - input 0: `audio` (Signal)
//! - output 0: `audio` (Signal) — low-pass output.
//! - param 0: `cutoff` (Hz)
//! - param 1: `resonance` (0..1)

use crate::descriptor::{Curve, Descriptor, ParamMeta, Port};
use crate::operator::{Io, Operator};

pub const IN_AUDIO: usize = 0;
pub const OUT_AUDIO: usize = 0;
pub const P_CUTOFF: usize = 0;
pub const P_RESONANCE: usize = 1;

#[derive(Default)]
pub struct Filter {
    /// State-variable integrator states.
    low: f32,
    band: f32,
}

impl Filter {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Filter {
    fn descriptor() -> Descriptor {
        Descriptor {
            type_name: "filter",
            inputs: vec![Port::signal("audio")],
            outputs: vec![Port::signal("audio")],
            params: vec![
                ParamMeta {
                    name: "cutoff",
                    min: 20.0,
                    max: 20_000.0,
                    default: 1_000.0,
                    unit: "Hz",
                    curve: Curve::Exponential,
                },
                ParamMeta {
                    name: "resonance",
                    min: 0.0,
                    max: 1.0,
                    default: 0.2,
                    unit: "",
                    curve: Curve::Linear,
                },
            ],
        }
    }

    fn process(&mut self, io: &mut Io) {
        // STAGE A STUB: pass through. Stage B implements the SVF low-pass.
        let n = io.frames();
        let input: Vec<f32> = io
            .input(IN_AUDIO)
            .map(|s| s.to_vec())
            .unwrap_or_else(|| vec![0.0; n]);
        let out = io.output(OUT_AUDIO);
        out[..n].copy_from_slice(&input[..n]);
        let _ = (&mut self.low, &mut self.band);
    }
}
