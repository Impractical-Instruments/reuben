//! Envelope — gated ADSR applied as a VCA.
//!
//! Ports/params are FROZEN (Stage A). DSP body is filled test-first in Stage B.
//!
//! - input 0: `audio` (Signal) — the signal to shape.
//! - input 1: `gate` (Signal) — > 0.5 means held; the rising/falling edge triggers A/R.
//! - output 0: `audio` (Signal) — `audio * env`.
//! - params 0..3: `attack`, `decay`, `sustain`, `release`.

use crate::descriptor::{Curve, Descriptor, ParamMeta, Port};
use crate::operator::{Io, Operator};

pub const IN_AUDIO: usize = 0;
pub const IN_GATE: usize = 1;
pub const OUT_AUDIO: usize = 0;
pub const P_ATTACK: usize = 0;
pub const P_DECAY: usize = 1;
pub const P_SUSTAIN: usize = 2;
pub const P_RELEASE: usize = 3;

#[derive(Default)]
pub struct Envelope {
    /// Current envelope level [0, 1].
    level: f32,
    /// Whether the gate was held on the previous sample.
    held: bool,
}

impl Envelope {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Envelope {
    fn descriptor() -> Descriptor {
        fn time(name: &'static str, default: f32) -> ParamMeta {
            ParamMeta {
                name,
                min: 0.001,
                max: 5.0,
                default,
                unit: "s",
                curve: Curve::Exponential,
            }
        }
        Descriptor {
            type_name: "envelope",
            inputs: vec![Port::signal("audio"), Port::signal("gate")],
            outputs: vec![Port::signal("audio")],
            params: vec![
                time("attack", 0.01),
                time("decay", 0.1),
                ParamMeta {
                    name: "sustain",
                    min: 0.0,
                    max: 1.0,
                    default: 0.7,
                    unit: "",
                    curve: Curve::Linear,
                },
                time("release", 0.2),
            ],
        }
    }

    fn process(&mut self, io: &mut Io) {
        // STAGE A STUB: pass audio through unchanged. Stage B implements ADSR.
        let n = io.frames();
        let input: Vec<f32> = io
            .input(IN_AUDIO)
            .map(|s| s.to_vec())
            .unwrap_or_else(|| vec![0.0; n]);
        let out = io.output(OUT_AUDIO);
        out[..n].copy_from_slice(&input[..n]);
        let _ = (&mut self.level, &mut self.held);
    }
}
