//! Oscillator — audio-rate tone generator.
//!
//! First operator migrated to the ADR-0028 **shape** model (the Phase 0 proof). It is
//! hand-written rather than declared via `operator_contract!` — the macro grows the new
//! `inputs { name: float { .. } }` surface in Phase 1; this operator demonstrates the engine
//! core (materialize + the single read path) underneath it.
//!
//! - input 0: `freq` (`Float`) — per-sample frequency in Hz. One declaration: when unwired the
//!   engine materializes it from the latched default (440 Hz) and writes mid-block `/osc/freq`
//!   changes at their frame; when wired (an LFO, a Voicer) the source buffer passes through. No
//!   more "signal port + same-named unwired-default param" pair, and no wired/unwired branch in
//!   `process` — `io.signal(IN_FREQ)` is always a buffer.
//! - input 1: `waveform` (`Float`) — 0.0 = sine, ≥0.5 = saw. Read block-rate via `io.value`.
//!   (Becomes an `Enum` input in the Phase 2 sweep; a `Float` here keeps Phase 0 scoped to the
//!   materialize discipline.)
//! - output 0: `audio` (`Float`).

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::operator::{Io, Operator};

/// `freq` input (materialized `Float`).
pub const IN_FREQ: usize = 0;
/// `waveform` input (materialized `Float`).
pub const IN_WAVEFORM: usize = 1;
/// `audio` output (`Float`).
pub const OUT_AUDIO: usize = 0;

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
            inputs: vec![
                Port::float(ParamMeta {
                    name: "freq",
                    min: 20.0,
                    max: 20_000.0,
                    default: 440.0,
                    unit: "Hz",
                    curve: Curve::Exponential,
                }),
                Port::float(ParamMeta {
                    name: "waveform",
                    min: 0.0,
                    max: 1.0,
                    default: 0.0,
                    unit: "",
                    curve: Curve::Linear,
                }),
            ],
            outputs: vec![Port::signal("audio")],
            params: vec![],
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

        // Waveform is a block-rate choice — one scalar read of the materialized value.
        let is_saw = io.value(IN_WAVEFORM) >= 0.5;

        // Stage 1: copy the per-sample frequency into the output buffer. `freq` is a `Float`
        // input, so it is always a buffer (wired source or materialized latch) — one read path,
        // no wired/unwired branch. Read per sample so the immutable input borrow ends before each
        // mutable output write (keeps `process` alloc-free without holding two borrows of `io`).
        for i in 0..n {
            let freq = io.signal(IN_FREQ).get(i).copied().unwrap_or(0.0);
            io.output(OUT_AUDIO)[i] = freq;
        }

        // Stage 2: in-place oscillator pass. `out[i]` currently holds the frequency for sample
        // `i`; overwrite it with the generated sample.
        let mut phase = self.phase;
        let out = &mut io.output(OUT_AUDIO)[..n];
        for slot in out.iter_mut() {
            let dt = *slot * inv_sr; // phase increment in turns

            let sample = if is_saw {
                // Naive saw in [-1, 1), with a polyBLEP correction at the wrap to reduce aliasing.
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

crate::register_operator!(Oscillator);

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
    use crate::message::Message;
    use crate::operator::Io;
    use crate::plan::Plan;
    use crate::render::Renderer;

    /// Run the oscillator over `n` frames in one `process` call. `freq`/`waveform` are supplied as
    /// the per-sample buffers the engine would materialize, so this exercises the operator's single
    /// read path directly.
    fn render_once(
        osc: &mut Oscillator,
        sample_rate: f32,
        n: usize,
        freq: f32,
        waveform: f32,
    ) -> Vec<f32> {
        let freq_buf = vec![freq; n];
        let wave_buf = vec![waveform; n];
        let mut o0 = vec![0.0f32; n];
        {
            let outs: Vec<&mut [f32]> = vec![&mut o0[..]];
            let inputs: Vec<Option<&[f32]>> = vec![Some(&freq_buf[..]), Some(&wave_buf[..])];
            let params: Vec<f32> = vec![];
            let mut io = Io::new(sample_rate, n, inputs, outs, &params, &[]);
            osc.process(&mut io);
        }
        o0
    }

    fn upward_crossings(buf: &[f32]) -> usize {
        let mut crossings = 0usize;
        let mut prev = 0.0f32;
        for &s in buf {
            if prev <= 0.0 && s > 0.0 {
                crossings += 1;
            }
            prev = s;
        }
        crossings
    }

    /// (1) A sine produces a tone at the requested frequency: ~440 upward crossings over ~1s, peak ~1.
    #[test]
    fn sine_tone_at_requested_frequency() {
        let sr = 48_000.0f32;
        let n = sr as usize; // ~1 second in a single call
        let mut osc = Oscillator::new();
        let out = render_once(&mut osc, sr, n, 440.0, 0.0);

        let crossings = upward_crossings(&out);
        assert!(
            (435..=445).contains(&crossings),
            "expected ~440 upward crossings, got {crossings}"
        );

        let peak = out.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        assert!(
            (0.98..=1.02).contains(&peak),
            "expected sine peak ~1.0, got {peak}"
        );
    }

    /// (2) Phase is continuous across two consecutive `process` calls (no click at the boundary).
    #[test]
    fn phase_is_continuous_across_calls() {
        let sr = 48_000.0f32;
        let n = 64;
        let freq = 440.0f32;
        let mut osc = Oscillator::new();

        let a = render_once(&mut osc, sr, n, freq, 0.0);
        let b = render_once(&mut osc, sr, n, freq, 0.0);

        let mut max_inblock = 0.0f32;
        for w in a.windows(2) {
            max_inblock = max_inblock.max((w[1] - w[0]).abs());
        }

        let boundary = (b[0] - a[n - 1]).abs();
        assert!(
            boundary <= max_inblock + 1e-4,
            "phase discontinuity at block boundary: boundary delta {boundary} \
             exceeds max in-block delta {max_inblock}"
        );
        assert!(
            (b[0] - a[0]).abs() > 1e-3,
            "second block appears to restart phase (b[0]={}, a[0]={})",
            b[0],
            a[0]
        );
    }

    /// (3) The per-sample `freq` buffer sets the pitch — a higher buffer value yields more crossings.
    #[test]
    fn freq_input_drives_pitch() {
        let sr = 48_000.0f32;
        let n = sr as usize;
        let mut osc = Oscillator::new();
        let out = render_once(&mut osc, sr, n, 880.0, 0.0);

        let crossings = upward_crossings(&out);
        assert!(
            (873..=887).contains(&crossings),
            "expected ~880 crossings from the freq buffer, got {crossings}"
        );
    }

    /// (4) Saw output rises monotonically within a period and spans ~[-1, 1].
    #[test]
    fn saw_ramps_and_spans_full_range() {
        let sr = 48_000.0f32;
        let freq = 100.0f32;
        let n = (sr / freq) as usize; // exactly one period worth of samples
        let mut osc = Oscillator::new();
        let out = render_once(&mut osc, sr, n, freq, 1.0);

        let min = out.iter().fold(f32::INFINITY, |m, &s| m.min(s));
        let max = out.iter().fold(f32::NEG_INFINITY, |m, &s| m.max(s));
        assert!(min < -0.9, "saw min should approach -1, got {min}");
        assert!(max > 0.9, "saw max should approach +1, got {max}");

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
        assert!(
            out[n - 1] > out[0],
            "saw should end higher than it starts (start {}, end {})",
            out[0],
            out[n - 1]
        );
    }

    /// Count upward zero crossings of logical channel 0 over `blocks` rendered blocks.
    fn render_crossings(
        plan: &mut Plan,
        r: &mut Renderer,
        cfg: AudioConfig,
        blocks: usize,
    ) -> usize {
        let mut crossings = 0usize;
        let mut prev = 0.0f32;
        let mut out = vec![0.0; cfg.block_size];
        for _ in 0..blocks {
            r.render_block(plan, &[], &mut out);
            for &s in &out {
                if prev <= 0.0 && s > 0.0 {
                    crossings += 1;
                }
                prev = s;
            }
        }
        crossings
    }

    /// (5) Materialize — held default. With nothing wired and no message, an oscillator renders its
    /// latched default (440 Hz): the engine fills the `freq` input buffer from the default scalar.
    #[test]
    fn materialized_default_produces_default_tone() {
        let cfg = AudioConfig::new(48_000.0, 512);
        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        g.tap_output(osc, OUT_AUDIO);
        let mut plan = Plan::instantiate(g, cfg).unwrap();
        let mut r = Renderer::new(&plan);

        let blocks = (cfg.sample_rate as usize) / cfg.block_size;
        let crossings = render_crossings(&mut plan, &mut r, cfg, blocks);
        assert!(
            (430..=450).contains(&crossings),
            "expected ~440 crossings from the materialized default, got {crossings}"
        );
    }

    /// (6) Materialize — literal override. `set_input` (the loader's `/osc/freq 220` path) seeds the
    /// latch, so the oscillator renders 220 Hz with nothing wired.
    #[test]
    fn materialized_override_sets_pitch() {
        let cfg = AudioConfig::new(48_000.0, 512);
        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        g.set_input(osc, "freq", 220.0);
        g.tap_output(osc, OUT_AUDIO);
        let mut plan = Plan::instantiate(g, cfg).unwrap();
        let mut r = Renderer::new(&plan);

        let blocks = (cfg.sample_rate as usize) / cfg.block_size;
        let crossings = render_crossings(&mut plan, &mut r, cfg, blocks);
        assert!(
            (215..=225).contains(&crossings),
            "expected ~220 crossings from the input override, got {crossings}"
        );
    }

    /// (7) Materialize — sample-accurate mid-block change. A `/osc/freq` message at frame N/2 in a
    /// single large block must take effect *at that frame*: the second half carries a much higher
    /// pitch than the first, in one `process` call (no block re-slicing for a `Float`).
    #[test]
    fn mid_block_freq_message_is_sample_accurate() {
        let sr = 48_000.0f32;
        let block = 9600usize; // 0.2 s — long enough to count crossings per half
        let cfg = AudioConfig::new(sr, block);
        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        g.set_input(osc, "freq", 200.0);
        g.tap_output(osc, OUT_AUDIO);
        let mut plan = Plan::instantiate(g, cfg).unwrap();
        let mut r = Renderer::new(&plan);

        // Change freq to 2000 Hz exactly at the half-block boundary.
        let half = block / 2;
        let msgs = [Message::float("/osc/freq", 2_000.0, half)];
        let mut out = vec![0.0f32; block];
        r.render_block(&mut plan, &msgs, &mut out);

        let first = upward_crossings(&out[..half]);
        let second = upward_crossings(&out[half..]);
        // 200 Hz over 0.1 s ≈ 20 crossings; 2000 Hz over 0.1 s ≈ 200.
        assert!(
            (16..=24).contains(&first),
            "first half should be ~200 Hz (~20 crossings), got {first}"
        );
        assert!(
            (190..=210).contains(&second),
            "second half should be ~2000 Hz (~200 crossings), got {second}"
        );
    }

    /// (8) Materialize — latch persists across blocks. A change in block 1 carries into block 2
    /// without re-sending the message (the latch is the held current value).
    #[test]
    fn latched_value_persists_across_blocks() {
        let sr = 48_000.0f32;
        let block = 4800usize; // 0.1 s
        let cfg = AudioConfig::new(sr, block);
        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        g.tap_output(osc, OUT_AUDIO);
        let mut plan = Plan::instantiate(g, cfg).unwrap();
        let mut r = Renderer::new(&plan);

        let mut out = vec![0.0f32; block];
        // Block 1: switch to 1000 Hz at frame 0.
        r.render_block(
            &mut plan,
            &[Message::float("/osc/freq", 1_000.0, 0)],
            &mut out,
        );
        // Block 2: no message — must stay at 1000 Hz.
        r.render_block(&mut plan, &[], &mut out);
        let crossings = upward_crossings(&out);
        // 1000 Hz over 0.1 s ≈ 100 crossings.
        assert!(
            (95..=105).contains(&crossings),
            "latched 1000 Hz should persist into block 2 (~100 crossings), got {crossings}"
        );
    }
}
