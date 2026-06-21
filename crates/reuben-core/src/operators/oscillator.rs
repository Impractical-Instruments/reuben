//! Oscillator — audio-rate tone generator.
//!
//! Ports/params are FROZEN (Stage A). DSP body is filled test-first in Stage B.
//!
//! - input 0: `freq` (Signal, optional) — per-sample frequency in Hz; overrides the param.
//! - output 0: `audio` (Signal)
//! - param 0: `freq` (Hz) — used when the freq input is unconnected.
//! - param 1: `waveform` — 0.0 = sine, 1.0 = saw.

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
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
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();
        let inv_sr = if sample_rate > 0.0 {
            1.0 / sample_rate
        } else {
            0.0
        };

        // Frequency source: per-sample input when connected, else the constant param.
        let freq_param = io.param(P_FREQ);
        let is_saw = io.param(P_WAVEFORM) >= 0.5;

        // Stage 1: fill the output buffer with the per-sample frequency. We read the
        // input port one sample at a time so its immutable borrow of `io` ends before
        // each mutable write to the output, then run the DSP pass in place. This keeps
        // `process` alloc-free.
        let freq_connected = io.input(IN_FREQ).is_some();
        for i in 0..n {
            let freq = if freq_connected {
                io.input(IN_FREQ).map_or(freq_param, |buf| buf[i])
            } else {
                freq_param
            };
            io.output(OUT_AUDIO)[i] = freq;
        }

        // Stage 2: in-place oscillator pass. `out[i]` currently holds the frequency
        // for sample `i`; we overwrite it with the generated sample.
        let mut phase = self.phase;
        let out = &mut io.output(OUT_AUDIO)[..n];
        for slot in out.iter_mut() {
            let dt = *slot * inv_sr; // phase increment in turns

            let sample = if is_saw {
                // Naive saw in [-1, 1), with a polyBLEP correction at the wrap to
                // reduce aliasing.
                let v = 2.0 * phase - 1.0;
                v - poly_blep(phase, dt)
            } else {
                (core::f32::consts::TAU * phase).sin()
            };
            *slot = sample;

            // Advance and wrap the phase accumulator (kept continuous across calls).
            phase += dt;
            phase -= phase.floor();
        }
        self.phase = phase;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

/// PolyBLEP residual for a sawtooth discontinuity at phase wrap (0/1 boundary).
///
/// `t` is the phase in turns [0, 1); `dt` is the per-sample phase increment.
/// Returns a correction to subtract from the naive ramp near the discontinuity.
fn poly_blep(t: f32, dt: f32) -> f32 {
    if dt <= 0.0 {
        return 0.0;
    }
    if t < dt {
        // Just after the wrap.
        let x = t / dt;
        2.0 * x - x * x - 1.0
    } else if t > 1.0 - dt {
        // Just before the wrap.
        let x = (t - 1.0) / dt;
        x * x + 2.0 * x + 1.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AudioConfig;
    use crate::graph::Graph;
    use crate::operator::Io;
    use crate::plan::Plan;
    use crate::render::Renderer;

    /// Run the oscillator over `n` frames in one `process` call and return the output.
    fn render_once(
        osc: &mut Oscillator,
        sample_rate: f32,
        n: usize,
        freq_input: Option<&[f32]>,
        freq_param: f32,
        waveform: f32,
    ) -> Vec<f32> {
        let mut o0 = vec![0.0f32; n];
        {
            let outs: Vec<&mut [f32]> = vec![&mut o0[..]];
            let inputs: Vec<Option<&[f32]>> = vec![freq_input];
            let params = vec![freq_param, waveform];
            let mut io = Io::new(sample_rate, n, inputs, outs, &params, &[]);
            osc.process(&mut io);
        }
        o0
    }

    /// (1) A sine produces a tone at the requested frequency: count upward zero
    /// crossings over ~1 second.
    #[test]
    fn sine_tone_at_requested_frequency() {
        let sr = 48_000.0f32;
        let n = sr as usize; // ~1 second in a single call
        let mut osc = Oscillator::new();
        let out = render_once(&mut osc, sr, n, None, 440.0, 0.0);

        let mut crossings = 0usize;
        let mut prev = 0.0f32;
        for &s in &out {
            if prev <= 0.0 && s > 0.0 {
                crossings += 1;
            }
            prev = s;
        }
        assert!(
            (435..=445).contains(&crossings),
            "expected ~440 upward crossings, got {crossings}"
        );

        // Sine peak should be ~1.0.
        let peak = out.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(
            (0.98..=1.02).contains(&peak),
            "expected sine peak ~1.0, got {peak}"
        );
    }

    /// (2) Phase is continuous across two consecutive `process` calls: the
    /// sample-to-sample delta at the boundary must be no larger than the deltas
    /// within a block (no click).
    #[test]
    fn phase_is_continuous_across_calls() {
        let sr = 48_000.0f32;
        let n = 64;
        let freq = 440.0f32;
        let mut osc = Oscillator::new();

        let a = render_once(&mut osc, sr, n, None, freq, 0.0);
        let b = render_once(&mut osc, sr, n, None, freq, 0.0);

        // Max in-block delta (a baseline for "smooth").
        let mut max_inblock = 0.0f32;
        for w in a.windows(2) {
            max_inblock = max_inblock.max((w[1] - w[0]).abs());
        }

        // Boundary delta between last sample of `a` and first of `b`.
        let boundary = (b[0] - a[n - 1]).abs();
        assert!(
            boundary <= max_inblock + 1e-4,
            "phase discontinuity at block boundary: boundary delta {boundary} \
             exceeds max in-block delta {max_inblock}"
        );

        // The second block must not simply restart from phase 0 (which would equal
        // the first block).
        assert!(
            (b[0] - a[0]).abs() > 1e-3,
            "second block appears to restart phase (b[0]={}, a[0]={})",
            b[0],
            a[0]
        );
    }

    /// (3) The freq INPUT overrides the freq param when connected.
    #[test]
    fn freq_input_overrides_param() {
        let sr = 48_000.0f32;
        let n = sr as usize;
        let input_freq = 880.0f32;
        let freq_buf = vec![input_freq; n];

        let mut osc = Oscillator::new();
        // Param says 100 Hz, but the connected input says 880 Hz.
        let out = render_once(&mut osc, sr, n, Some(&freq_buf[..]), 100.0, 0.0);

        let mut crossings = 0usize;
        let mut prev = 0.0f32;
        for &s in &out {
            if prev <= 0.0 && s > 0.0 {
                crossings += 1;
            }
            prev = s;
        }
        assert!(
            (873..=887).contains(&crossings),
            "expected ~880 crossings from input override, got {crossings}"
        );
    }

    /// (4) Saw output rises monotonically within a period and spans ~[-1, 1].
    #[test]
    fn saw_ramps_and_spans_full_range() {
        let sr = 48_000.0f32;
        let freq = 100.0f32;
        let n = (sr / freq) as usize; // exactly one period worth of samples
        let mut osc = Oscillator::new();
        let out = render_once(&mut osc, sr, n, None, freq, 1.0);

        // Span roughly [-1, 1].
        let min = out.iter().fold(f32::INFINITY, |m, &s| m.min(s));
        let max = out.iter().fold(f32::NEG_INFINITY, |m, &s| m.max(s));
        assert!(min < -0.9, "saw min should approach -1, got {min}");
        assert!(max > 0.9, "saw max should approach +1, got {max}");

        // Monotonically rising through the interior of the period. The polyBLEP
        // anti-aliasing correction bends the ramp within one sample of the wrap
        // (the very first and last samples here), so check the interior only.
        let mut non_increasing = 0usize;
        for w in out[1..n - 1].windows(2) {
            if w[1] < w[0] - 1e-4 {
                non_increasing += 1;
            }
        }
        assert!(
            non_increasing == 0,
            "saw should rise monotonically within a period, {non_increasing} drops"
        );

        // Overall the waveform must trend strongly upward across the period.
        assert!(
            out[n - 1] > out[0],
            "saw should end higher than it starts (start {}, end {})",
            out[0],
            out[n - 1]
        );
    }

    /// Render a steady 440 Hz tone and count zero crossings; expect ~one period per
    /// `sample_rate / 440` samples.
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
