//! Oscillator — audio-rate tone generator.
//!
//! - input 0: `freq` (`Float`) — per-sample frequency in Hz. One declaration: when unwired the
//!   engine materializes it from the latched default (440 Hz) and writes mid-block `/osc/freq`
//!   changes at their frame; when wired (an LFO, a Voicer) the source buffer passes through, so
//!   `io.input::<&[f32]>(IN_FREQ)` is always a buffer.
//! - input 1: `waveform` (`Enum` [`Waveform`] {Sine, Saw}) — held, live-switchable choice read
//!   via `io.input::<Waveform>` (ADR-0030).
//! - output 0: `audio` (`Buffer`).

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::vocab::Waveform;
use crate::wavetable::{shared_sine, Wavetable};

// Single-source contract (ADR-0025/0030): one declaration -> IN_/OUT_ consts and the Descriptor.
// `freq` is a signal control with a scalar default (ADR-0031 decision (a)): knob-set or unwired it
// materializes from 440 Hz, yet an LFO/envelope Signal wires straight in. `waveform` references the
// shared `Waveform` vocab enum.
crate::operator_contract!(Oscillator {
    inputs:  { freq:     f32_buffer { 20.0..=20_000.0, default 440.0, "Hz", exp },
               waveform: enum(Waveform) },
    outputs: { audio: f32_buffer },
});

pub struct Oscillator {
    /// Phase in turns [0, 1).
    phase: f32,
    /// Shared single-cycle sine table — the sine waveform is a phase-indexed lookup with linear
    /// interpolation rather than a per-sample `sin()` call. Resolved here on the cold path so
    /// `process` only ever reads the already-built table (ADR-0019 RT-safe render).
    sine: &'static Wavetable,
}

impl Oscillator {
    pub fn new() -> Self {
        Self {
            phase: 0.0,
            sine: shared_sine(),
        }
    }
}

impl Default for Oscillator {
    fn default() -> Self {
        Self::new()
    }
}

impl Operator for Oscillator {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();
        let inv_sr = if sample_rate > 0.0 {
            1.0 / sample_rate
        } else {
            0.0
        };

        // Waveform is a held `Enum` choice (ADR-0030) — one read, constant for this call.
        let is_saw = io.input::<Waveform>(IN_WAVEFORM).unwrap_or_default() == Waveform::Saw;

        // Stage 1: copy the per-sample frequency into the output buffer. `freq` is a `Float`
        // input, so it is always a buffer (wired source or materialized latch) — one read path,
        // no wired/unwired branch. Read per sample so the immutable input borrow ends before each
        // mutable output write (keeps `process` alloc-free without holding two borrows of `io`).
        for i in 0..n {
            let freq = io.input::<&[f32]>(IN_FREQ).get(i).copied().unwrap_or(0.0);
            io.output::<&mut [f32]>(OUT_AUDIO)[i] = freq;
        }

        // Stage 2: in-place oscillator pass. `out[i]` currently holds the frequency for sample
        // `i`; overwrite it with the generated sample.
        let mut phase = self.phase;
        let sine = self.sine; // `&'static`, copied out so the loop doesn't hold a borrow of `self`
        let out = &mut io.output::<&mut [f32]>(OUT_AUDIO)[..n];
        for slot in out.iter_mut() {
            let dt = *slot * inv_sr; // phase increment in turns

            let sample = if is_saw {
                // Naive saw in [-1, 1), with a polyBLEP correction at the wrap to reduce aliasing.
                let v = 2.0 * phase - 1.0;
                v - poly_blep(phase, dt)
            } else {
                // Sine by phase-indexed wavetable lookup (linear interpolation) — no per-sample
                // trig. `phase` is kept in [0, 1) by the accumulator below, so it meets `lookup`'s
                // contract every iteration.
                sine.lookup(phase)
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
    use crate::message::Arg;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Drive a fresh oscillator for `n` frames at the given constant `freq`/`waveform` through the
    /// real engine, returning the audio buffer. `freq` is a held `Float` control (materialized,
    /// `set` once); `waveform` is a held `Enum` choice. The phase threads across the real 128-frame
    /// blocks, so the tone stays continuous across them.
    fn run(n: usize, freq: f32, waveform: Waveform) -> Vec<f32> {
        OpDriver::for_type(Oscillator::new(), SR)
            .set(IN_FREQ, freq)
            .set(IN_WAVEFORM, waveform)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec()
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
        let n = SR as usize; // ~1 second
        let out = run(n, 440.0, Waveform::Sine);

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

    /// (1b) ADR-0031 decision (a): `freq` is now a signal port (`f32_buffer`) *carrying* a scalar
    /// default. With **no** override wired or knob-set, the engine must still materialize the buffer
    /// from that default (440 Hz) — not an empty/zero buffer. Drive it without `set(IN_FREQ, ..)` and
    /// assert the same ~440 tone.
    #[test]
    fn unwired_freq_materializes_its_440_default() {
        let n = SR as usize; // ~1 second
        let out = OpDriver::for_type(Oscillator::new(), SR)
            .set(IN_WAVEFORM, Waveform::Sine)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        let crossings = upward_crossings(&out);
        assert!(
            (435..=445).contains(&crossings),
            "unwired freq should default to ~440 Hz, got {crossings} crossings"
        );
    }

    /// (2) Phase is continuous across two consecutive `render` calls (no click at the boundary):
    /// the phase threads across the real blocks and across the separate calls sharing the operator.
    #[test]
    fn phase_is_continuous_across_calls() {
        let n = 64;
        let freq = 440.0f32;
        let mut osc = OpDriver::for_type(Oscillator::new(), SR);
        osc.set(IN_FREQ, freq).set(IN_WAVEFORM, Waveform::Sine);

        let a = osc.render(n).output(OUT_AUDIO).to_vec();
        let b = osc.render(n).output(OUT_AUDIO).to_vec();

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

    /// (3) The `freq` input sets the pitch — a higher value yields more crossings.
    #[test]
    fn freq_input_drives_pitch() {
        let n = SR as usize;
        let out = run(n, 880.0, Waveform::Sine);

        let crossings = upward_crossings(&out);
        assert!(
            (873..=887).contains(&crossings),
            "expected ~880 crossings from the freq buffer, got {crossings}"
        );
    }

    /// (4) Saw output rises monotonically within a period and spans ~[-1, 1].
    #[test]
    fn saw_ramps_and_spans_full_range() {
        let freq = 100.0f32;
        let n = (SR / freq) as usize; // exactly one period worth of samples
        let out = run(n, freq, Waveform::Saw);

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

    /// (5) Materialize — held default. With nothing set, an oscillator renders its latched default
    /// (440 Hz): the engine fills the `freq` input buffer from the default scalar.
    #[test]
    fn materialized_default_produces_default_tone() {
        let n = SR as usize; // 1 second
        let out = OpDriver::for_type(Oscillator::new(), SR)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();
        let crossings = upward_crossings(&out);
        assert!(
            (430..=450).contains(&crossings),
            "expected ~440 crossings from the materialized default, got {crossings}"
        );
    }

    /// (6) Materialize — literal override. `set` (the loader's `/osc/freq 220` path) seeds the
    /// latch, so the oscillator renders 220 Hz with nothing wired.
    #[test]
    fn materialized_override_sets_pitch() {
        let n = SR as usize; // 1 second
        let out = run(n, 220.0, Waveform::Sine);
        let crossings = upward_crossings(&out);
        assert!(
            (215..=225).contains(&crossings),
            "expected ~220 crossings from the input override, got {crossings}"
        );
    }

    /// (7) Materialize — sample-accurate mid-render change. A `/osc/freq` message at frame N/2 must
    /// take effect *at that frame*: the second half carries a much higher pitch than the first.
    #[test]
    fn mid_block_freq_message_is_sample_accurate() {
        let n = 9600usize; // 0.2 s — long enough to count crossings per half
        let half = n / 2;
        // Start at 200 Hz; change to 2000 Hz exactly at the half-render boundary (frame `half`).
        let out = OpDriver::for_type(Oscillator::new(), SR)
            .set(IN_FREQ, 200.0)
            .push(IN_FREQ, half, 2_000.0)
            .render(n)
            .output(OUT_AUDIO)
            .to_vec();

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

    /// (8) Materialize — latch persists across blocks. A change at frame 0 carries through later
    /// blocks without re-sending the message (the latch is the held current value).
    #[test]
    fn latched_value_persists_across_blocks() {
        let block = 4800usize; // 0.1 s
                               // Switch to 1000 Hz at frame 0, then render two blocks' worth; the second block carries
                               // no further message and must stay at 1000 Hz.
        let out = OpDriver::for_type(Oscillator::new(), SR)
            .push(IN_FREQ, 0, 1_000.0)
            .render(2 * block)
            .output(OUT_AUDIO)
            .to_vec();
        let crossings = upward_crossings(&out[block..]);
        // 1000 Hz over 0.1 s ≈ 100 crossings.
        assert!(
            (95..=105).contains(&crossings),
            "latched 1000 Hz should persist into block 2 (~100 crossings), got {crossings}"
        );
    }

    /// Count the fraction of consecutive samples that rise — ~1.0 for a saw ramp, ~0.5 for a sine.
    fn rising_fraction(buf: &[f32]) -> f32 {
        let rising = buf.windows(2).filter(|w| w[1] > w[0]).count();
        rising as f32 / (buf.len() - 1) as f32
    }

    /// (9) Enum delivery, end-to-end (ADR-0030). The default `waveform` is `Sine`; a live
    /// `/osc/waveform "Saw"` message (resolved by symbol through the engine's enum route + latch)
    /// switches the shape to a near-monotonic ramp, and the latch persists into the next block.
    #[test]
    fn waveform_enum_switches_live_via_message() {
        let block = 4800usize; // 0.1 s; 100 Hz → 10 long periods per block
                               // Switch to Saw by symbol at frame `block` (start of block 2). Block 1 is the default
                               // Sine; block 3 carries no message, so the enum latch must persist as Saw.
        let out = OpDriver::for_type(Oscillator::new(), SR)
            .set(IN_FREQ, 100.0)
            .push(IN_WAVEFORM, block, Arg::Str("Saw".into()))
            .render(3 * block)
            .output(OUT_AUDIO)
            .to_vec();
        let sine = &out[..block];
        let saw = &out[block..2 * block];
        let saw2 = &out[2 * block..];

        assert!(
            (0.4..=0.6).contains(&rising_fraction(sine)),
            "default waveform should be a sine, rising frac {}",
            rising_fraction(sine)
        );
        assert!(
            rising_fraction(saw) > 0.9,
            "Saw should be a near-monotonic ramp, rising frac {}",
            rising_fraction(saw)
        );
        assert!(
            rising_fraction(saw2) > 0.9,
            "Saw latch should persist into block 3, rising frac {}",
            rising_fraction(saw2)
        );
    }
}
