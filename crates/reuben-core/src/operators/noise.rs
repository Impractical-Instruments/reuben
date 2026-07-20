//! Noise — white-noise Signal source (V1.3).
//!
//! A zero-input audio-rate generator emitting uniform white noise in ~[-1, 1]. The percussive
//! core of the synthesized drums (snare body, hat) — a noise burst through an envelope (and,
//! for the hat, a highpass `filter`). The PRNG is a tiny inline **xorshift32** seeded in the
//! struct, advanced once per sample: no allocation, no `rand` dependency, RT-safe. The RNG
//! state lives in the struct, so the stream is continuous across blocks / block-slices (no
//! audible seam at a block boundary). `spawn` resets to a fixed deterministic seed, so a fresh
//! Voice always starts from the same point — reproducible renders.
//!
//! - inputs: none.
//! - output 0: `out` (`Buffer`) — uniform white noise in ~[-1, 1], roughly zero-mean.
//! - params: none.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract: one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Noise {
    outputs: { out: f32_buffer },
});

/// Fixed deterministic seed a fresh / spawned Noise starts from. Non-zero (xorshift can't leave
/// the zero state). An arbitrary odd constant; the exact value only matters for reproducibility.
const SEED: u32 = 0x2545_F491;

pub struct Noise {
    /// xorshift32 state, advanced once per sample. Non-zero invariant. Continuous across blocks;
    /// reset to `SEED` on `spawn`.
    rng: u32,
}

impl Default for Noise {
    fn default() -> Self {
        Self { rng: SEED }
    }
}

impl Noise {
    pub fn new() -> Self {
        Self::default()
    }

    /// One xorshift32 step → a fresh u32. Marsaglia's classic (13, 17, 5) triple; full period
    /// over the non-zero u32s, so the state never collapses to 0.
    #[inline]
    fn next_u32(&mut self) -> u32 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.rng = x;
        x
    }

    /// Next white-noise sample, uniform in [-1, 1). Maps the u32 to a float by taking the top
    /// 24 bits (the f32 mantissa width) so the distribution is even and allocation-free.
    #[inline]
    fn next_sample(&mut self) -> f32 {
        let bits = self.next_u32() >> 8; // top 24 bits → [0, 2^24)
                                         // [0, 1) then to [-1, 1): 2*u - 1.
        (bits as f32) * (1.0 / (1u32 << 24) as f32) * 2.0 - 1.0
    }
}

impl Operator for Noise {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let out = io.write(OUT_OUT);
        for s in out.iter_mut().take(n) {
            *s = self.next_sample();
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Noise);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Drive a fresh `Noise` for `n` frames through the real engine, returning the out buffer.
    /// No inputs/params: the PRNG free-runs from `SEED` (Plan instantiation preserves the seed),
    /// advancing one step per sample continuously across the real 128-frame blocks.
    fn run(n: usize) -> Vec<f32> {
        OpDriver::for_type(Noise::new(), SR)
            .render(n)
            .output(OUT_OUT)
            .to_vec()
    }

    #[test]
    fn output_is_bounded_and_finite() {
        let out = run(48_000);
        for (i, &s) in out.iter().enumerate() {
            assert!(s.is_finite(), "sample {i} not finite: {s}");
            assert!((-1.0..1.0).contains(&s), "sample {i} out of [-1,1): {s}");
        }
    }

    #[test]
    fn output_is_non_constant() {
        // White noise must vary — the buffer is not a single repeated value.
        let out = run(1_000);
        let first = out[0];
        assert!(
            out.iter().any(|&s| s != first),
            "noise output is constant ({first}) — PRNG not advancing"
        );
    }

    #[test]
    fn output_is_roughly_zero_mean() {
        // Over a long buffer, uniform [-1,1) noise averages near 0.
        let out = run(200_000);
        let mean = out.iter().sum::<f32>() / out.len() as f32;
        assert!(mean.abs() < 0.01, "mean {mean} should be near 0");
    }

    #[test]
    fn state_is_continuous_across_calls() {
        // One render of 2n must equal two back-to-back renders of n sharing the driver's operator:
        // the RNG state carries across the real block boundaries and across separate `render` calls
        // (no seam).
        let n = 1000;
        let w = run(2 * n);

        let mut split = OpDriver::for_type(Noise::new(), SR);
        let a = split.render(n).output(OUT_OUT).to_vec();
        let b = split.render(n).output(OUT_OUT).to_vec();

        for i in 0..n {
            assert_eq!(a[i].to_bits(), w[i].to_bits(), "block 1 differs at {i}");
            assert_eq!(b[i].to_bits(), w[n + i].to_bits(), "block 2 differs at {i}");
        }
    }

    #[test]
    fn spawned_noise_resets_to_deterministic_seed() {
        // A spawn starts from the fixed seed regardless of how far the source advanced, so its
        // stream is bit-identical to a fresh instance's.
        let mut a = OpDriver::for_type(Noise::new(), SR);
        a.render(5_000);

        let fresh_out = run(1_000);
        let spawned_out = a.spawn().render(1_000).output(OUT_OUT).to_vec();

        for i in 0..1_000 {
            assert_eq!(
                spawned_out[i].to_bits(),
                fresh_out[i].to_bits(),
                "spawned noise should match a fresh instance at {i}"
            );
        }
    }

    #[test]
    fn two_streams_can_be_made_independent() {
        // Advancing one instance past the other yields a different (decorrelated) stream — the
        // generator is stateful, not a pure function of frame index. (Drum Voices that need
        // distinct noise seed differently; the spawn reset is the deterministic default.)
        let mut a = OpDriver::for_type(Noise::new(), SR);
        a.render(7); // offset this stream by 7 samples
        let b_out = run(1_000);
        let a_out = a.render(1_000).output(OUT_OUT).to_vec();
        assert!(
            a_out.iter().zip(&b_out).filter(|(x, y)| x != y).count() > 900,
            "an offset stream should differ from a fresh one at most frames"
        );
    }
}
