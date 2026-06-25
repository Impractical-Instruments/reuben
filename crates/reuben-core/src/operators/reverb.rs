//! Reverb — mono Freeverb (Schroeder/Moorer): 8 parallel lowpass-feedback comb
//! filters summed, then 4 series allpass filters.
//!
//! Single-Lane (mono in, mono out).
//!
//! Port types (ADR-0030): `room`/`damp`/`mix` are **`F32` inputs**, each owning its unwired
//! default. When nothing is wired the engine materializes the input from its latched default;
//! when a control is wired the source buffer passes through. There is no longer a separate
//! "signal port + same-named param" pair — `io.last::<f32>(IN_ROOM)` reads the latched value.
//!
//! - input 0: `audio` (`Float`)
//! - input 1: `room` (`Float`) — room size / tail length (0..1).
//! - input 2: `damp` (`Float`) — high-frequency damping (0..1).
//! - input 3: `mix` (`Float`) — dry/wet (0..1).
//! - output 0: `audio` (`Float`) — dry+wet mix.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025/0028): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Reverb {
    inputs:  { audio: buffer,
               room: float { 0.0..=1.0, default 0.5, "", lin },
               damp: float { 0.0..=1.0, default 0.5, "", lin },
               mix:  float { 0.0..=1.0, default 0.3, "", lin } },
    outputs: { audio: buffer },
});

/// Standard Freeverb comb-filter delay lengths, in samples at 44100 Hz.
const COMB_TUNING: [usize; 8] = [1116, 1188, 1277, 1356, 1422, 1491, 1557, 1617];
/// Standard Freeverb allpass-filter delay lengths, in samples at 44100 Hz.
const ALLPASS_TUNING: [usize; 4] = [556, 441, 341, 225];

/// Fixed input gain that keeps the comb bank well-conditioned.
const FIXED_GAIN: f32 = 0.015;

/// A single lowpass-feedback comb filter (one of the 8 parallel branches).
#[derive(Default)]
struct Comb {
    buffer: Vec<f32>,
    pos: usize,
    /// One-pole lowpass state in the feedback path (the "damping" filter).
    filter_store: f32,
}

impl Comb {
    fn resize(&mut self, len: usize) {
        self.buffer = vec![0.0; len];
        self.pos = 0;
        self.filter_store = 0.0;
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32, damp: f32) -> f32 {
        let output = self.buffer[self.pos];
        // One-pole lowpass on the feedback signal (high-frequency damping).
        self.filter_store = output * (1.0 - damp) + self.filter_store * damp;
        self.buffer[self.pos] = input + self.filter_store * feedback;
        self.pos += 1;
        if self.pos >= self.buffer.len() {
            self.pos = 0;
        }
        output
    }
}

/// A single allpass filter (one of the 4 series stages).
#[derive(Default)]
struct Allpass {
    buffer: Vec<f32>,
    pos: usize,
}

impl Allpass {
    fn resize(&mut self, len: usize) {
        self.buffer = vec![0.0; len];
        self.pos = 0;
    }

    #[inline]
    fn process(&mut self, input: f32, feedback: f32) -> f32 {
        let buffered = self.buffer[self.pos];
        let output = -input + buffered;
        self.buffer[self.pos] = input + buffered * feedback;
        self.pos += 1;
        if self.pos >= self.buffer.len() {
            self.pos = 0;
        }
        output
    }
}

#[derive(Default)]
pub struct Reverb {
    /// 8 parallel lowpass-feedback comb filters (allocated lazily on first process).
    combs: [Comb; 8],
    /// 4 series allpass filters (allocated lazily on first process).
    allpasses: [Allpass; 4],
    /// Sample rate the delay buffers were sized for; 0.0 until the first process.
    sized_rate: f32,
}

impl Reverb {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scale a 44100 Hz tuning to `sample_rate` and round to whole samples (min 1).
    fn scaled(len: usize, sample_rate: f32) -> usize {
        ((len as f32 * sample_rate / 44_100.0).round() as usize).max(1)
    }
}

impl Operator for Reverb {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        // Lazily (re)size the delay buffers when the sample rate is known/changed. This is
        // rt-safe: in steady state the rate is constant, so nothing allocates.
        if self.sized_rate != sample_rate {
            for (comb, &len) in self.combs.iter_mut().zip(COMB_TUNING.iter()) {
                comb.resize(Self::scaled(len, sample_rate));
            }
            for (ap, &len) in self.allpasses.iter_mut().zip(ALLPASS_TUNING.iter()) {
                ap.resize(Self::scaled(len, sample_rate));
            }
            self.sized_rate = sample_rate;
        }

        // `room`/`damp`/`mix` are `Float` inputs — read the latched block-rate value (ADR-0030).
        let room = io.last::<f32>(IN_ROOM).unwrap_or(0.0).clamp(0.0, 1.0);
        let damp = io.last::<f32>(IN_DAMP).unwrap_or(0.0).clamp(0.0, 1.0);
        let mix = io.last::<f32>(IN_MIX).unwrap_or(0.0).clamp(0.0, 1.0);

        // Standard Freeverb parameter mappings.
        let roomsize = room * 0.28 + 0.7; // comb feedback
        let damp_coeff = damp * 0.4;
        let allpass_feedback = 0.5;
        let wet = mix;
        let dry = 1.0 - mix;

        for i in 0..n {
            let dry_in = io.signal(IN_AUDIO).get(i).copied().unwrap_or(0.0);
            let input = dry_in * FIXED_GAIN;

            // 8 parallel comb filters summed.
            let mut wet_sig = 0.0;
            for comb in &mut self.combs {
                wet_sig += comb.process(input, roomsize, damp_coeff);
            }

            // 4 series allpass filters.
            for ap in &mut self.allpasses {
                wet_sig = ap.process(wet_sig, allpass_feedback);
            }

            io.signal_mut(OUT_AUDIO)[i] = dry_in * dry + wet_sig * wet;
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Reverb);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Arg;
    use crate::operator::Io;

    /// Run `input` through a fresh Reverb at the given room/damp/mix and return the output.
    /// `room`/`damp`/`mix` are `Float` inputs read via `io.last` (ADR-0030), supplied as the
    /// per-input held (ZOH) Args in port order (audio, room, damp, mix); `audio` is a buffer
    /// input (placeholder latch).
    fn render(input: &[f32], sample_rate: f32, room: f32, damp: f32, mix: f32) -> Vec<f32> {
        let n = input.len();
        let mut reverb = Reverb::new();
        let mut out_buf = vec![0.0f32; n];

        let latched = [Arg::F32(0.0), Arg::F32(room), Arg::F32(damp), Arg::F32(mix)];
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(input), None, None, None];
            let outputs: Vec<&mut [f32]> = vec![out_buf.as_mut_slice()];
            let mut io = Io::new(sample_rate, n, inputs, outputs).with_latched(&latched);
            reverb.process(&mut io);
        }
        out_buf
    }

    fn rms(buf: &[f32]) -> f32 {
        let sum: f32 = buf.iter().map(|x| x * x).sum();
        (sum / buf.len() as f32).sqrt()
    }

    /// A single-sample impulse followed by `n-1` zeros.
    fn impulse(n: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; n];
        v[0] = 1.0;
        v
    }

    #[test]
    fn impulse_produces_a_decaying_tail_well_beyond_the_input() {
        let sr = 48_000.0;
        let n = sr as usize; // 1 second
        let out = render(&impulse(n), sr, 0.7, 0.5, 1.0);

        // Energy is still present hundreds of ms after the single-sample input.
        let win = |start_ms: f32, len_ms: f32| {
            let a = (sr * start_ms / 1000.0) as usize;
            let b = (a + (sr * len_ms / 1000.0) as usize).min(n);
            rms(&out[a..b])
        };
        let early = win(20.0, 50.0);
        let late = win(400.0, 50.0);
        assert!(early > 0.0, "no early reverb energy");
        assert!(late > 0.0, "no late reverb energy ({late}) at 400 ms");
        // It's a decaying tail: late energy is below the early energy.
        assert!(
            late < early,
            "tail should decay (early {early}, late {late})"
        );
    }

    #[test]
    fn output_stays_finite_and_bounded_under_extreme_settings() {
        let sr = 48_000.0;
        let n = 4 * sr as usize; // 4 seconds of full-scale noise-ish drive
                                 // Alternating full-scale input is a harsh worst case for feedback combs.
        let input: Vec<f32> = (0..n)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let out = render(&input, sr, 1.0, 0.0, 1.0);

        for (i, &s) in out.iter().enumerate() {
            assert!(s.is_finite(), "sample {i} not finite: {s}");
            assert!(s.abs() < 16.0, "sample {i} unbounded: {s}");
        }
    }

    #[test]
    fn mix_zero_is_dry_passthrough() {
        let sr = 48_000.0;
        let n = 2048;
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 220.0 * i as f32 / sr).sin())
            .collect();
        let out = render(&input, sr, 0.8, 0.5, 0.0);

        for i in 0..n {
            assert!(
                (out[i] - input[i]).abs() < 1e-6,
                "mix=0 should be dry at {i}: {} vs {}",
                out[i],
                input[i]
            );
        }
    }

    #[test]
    fn larger_room_yields_a_longer_louder_late_tail() {
        let sr = 48_000.0;
        let n = sr as usize; // 1 second
        let small = render(&impulse(n), sr, 0.1, 0.5, 1.0);
        let large = render(&impulse(n), sr, 0.95, 0.5, 1.0);

        // Measure a late window (~500 ms in) where tail length matters most.
        let a = (sr * 0.5) as usize;
        let b = (a + (sr * 0.1) as usize).min(n);
        let small_late = rms(&small[a..b]);
        let large_late = rms(&large[a..b]);

        assert!(
            large_late > small_late * 1.5,
            "larger room should have a louder late tail (small {small_late}, large {large_late})"
        );
    }

    #[test]
    fn state_continuous_across_block_slices() {
        // Processing one buffer in one call must equal processing it in two calls that
        // share the same Reverb instance.
        let sr = 48_000.0;
        let n = 1024;
        let input: Vec<f32> = (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * 330.0 * i as f32 / sr).sin())
            .collect();

        let whole = render(&input, sr, 0.6, 0.4, 0.5);

        let mut reverb = Reverb::new();
        let mut out_buf = vec![0.0f32; n];
        let latched = [Arg::F32(0.0), Arg::F32(0.6), Arg::F32(0.4), Arg::F32(0.5)];
        let half = n / 2;
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input[..half]), None, None, None];
            let outputs: Vec<&mut [f32]> = vec![&mut out_buf[..half]];
            let mut io = Io::new(sr, half, inputs, outputs).with_latched(&latched);
            reverb.process(&mut io);
        }
        {
            let inputs: Vec<Option<&[f32]>> = vec![Some(&input[half..]), None, None, None];
            let outputs: Vec<&mut [f32]> = vec![&mut out_buf[half..]];
            let mut io = Io::new(sr, n - half, inputs, outputs).with_latched(&latched);
            reverb.process(&mut io);
        }

        for i in 0..n {
            assert!(
                (whole[i] - out_buf[i]).abs() < 1e-5,
                "slice mismatch at {i}: {} vs {}",
                whole[i],
                out_buf[i]
            );
        }
    }
}
