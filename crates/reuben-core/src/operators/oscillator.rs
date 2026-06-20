//! Oscillator — audio-rate tone generator.
//!
//! Ports/params are FROZEN (Stage A). DSP body is filled test-first in Stage B.
//!
//! - input 0: `freq` (Signal, optional) — per-sample frequency in Hz; overrides the param.
//! - output 0: `audio` (Signal)
//! - param 0: `freq` (Hz) — used when the freq input is unconnected.
//! - param 1: `waveform` — 0.0 = sine, 1.0 = saw.

use crate::descriptor::{Curve, Descriptor, ParamMeta, Port};
use crate::operator::{Io, Operator};

pub const IN_FREQ: usize = 0;
pub const OUT_AUDIO: usize = 0;
pub const P_FREQ: usize = 0;
pub const P_WAVEFORM: usize = 1;

#[derive(Default)]
pub struct Oscillator {
    /// Phase in turns [0, 1).
    phase: f32,
}

impl Oscillator {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Oscillator {
    fn descriptor() -> Descriptor {
        Descriptor {
            type_name: "oscillator",
            inputs: vec![Port::signal("freq")],
            outputs: vec![Port::signal("audio")],
            params: vec![
                ParamMeta {
                    name: "freq",
                    min: 20.0,
                    max: 20_000.0,
                    default: 440.0,
                    unit: "Hz",
                    curve: Curve::Exponential,
                },
                ParamMeta {
                    name: "waveform",
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    unit: "",
                    curve: Curve::Linear,
                },
            ],
        }
    }

    fn process(&mut self, io: &mut Io) {
        // STAGE A STUB: silent. Stage B implements the oscillator (this makes the
        // `produces_tone` test below go from red to green).
        let n = io.frames();
        let out = io.output(OUT_AUDIO);
        out[..n].iter_mut().for_each(|s| *s = 0.0);
        let _ = &mut self.phase;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AudioConfig;
    use crate::graph::Graph;
    use crate::plan::Plan;
    use crate::render::Renderer;

    /// Render a steady 440 Hz tone and count zero crossings; expect ~one period per
    /// `sample_rate / 440` samples. RED until the oscillator DSP lands in Stage B.
    #[test]
    fn produces_tone() {
        let cfg = AudioConfig::new(48_000.0, 512);
        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        g.set_param(osc, "freq", 440.0);
        g.tap_output(osc, OUT_AUDIO);
        let mut plan = Plan::instantiate(g, cfg).unwrap();
        let mut r = Renderer::new(&plan);

        // Render ~1 second.
        let blocks = (cfg.sample_rate as usize) / cfg.block_size;
        let mut crossings = 0usize;
        let mut prev = 0.0f32;
        let mut out = vec![0.0; cfg.block_size];
        for _ in 0..blocks {
            r.render_block(&mut plan, &[], &mut out);
            for &s in &out {
                if prev <= 0.0 && s > 0.0 {
                    crossings += 1;
                }
                prev = s;
            }
        }
        // ~440 upward crossings per second, allow generous tolerance.
        assert!(
            (430..=450).contains(&crossings),
            "expected ~440 zero crossings, got {crossings}"
        );
    }
}
