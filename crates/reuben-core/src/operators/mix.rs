//! mix — sum up to sixteen mono Signals, each through its own gain, into one Signal.
//!
//! `out[i] = Σₖ inₖ[i] · gainₖ[i]` for k = 1..=16, summed in port order so the result is
//! bit-identical across runs. A gain is read per sample, so an envelope or follower can ride a
//! channel level directly. A gain at or below 0, or NaN, mutes its source outright (even an
//! inf or NaN source); above 4 it is held at 4. A bus of N sources wires `in1`..`inN` and leaves
//! the rest unwired: an unwired Signal input reads silence, so it contributes nothing. A stereo bus
//! is two `mix` nodes, one per side.
//!
//! The width is fixed because a [`Descriptor`] is a function of the operator type, not the node —
//! there is no config-sized port set to size it per bus.

use alloc::boxed::Box;

use crate::descriptor::Descriptor;
use crate::operator::form::SignalF32;
use crate::operator::{In, Io, Operator};

// Single-source contract: one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Mix {
    type_name: "mix",
    inputs: { in1: f32_buffer, in2: f32_buffer, in3: f32_buffer, in4: f32_buffer,
              in5: f32_buffer, in6: f32_buffer, in7: f32_buffer, in8: f32_buffer,
              in9: f32_buffer, in10: f32_buffer, in11: f32_buffer, in12: f32_buffer,
              in13: f32_buffer, in14: f32_buffer, in15: f32_buffer, in16: f32_buffer,
              gain1: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain2: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain3: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain4: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain5: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain6: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain7: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain8: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain9: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain10: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain11: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain12: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain13: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain14: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain15: f32_buffer { 0.0..=4.0, default 1.0, "", lin },
              gain16: f32_buffer { 0.0..=4.0, default 1.0, "", lin } },
    outputs: { out: f32_buffer },
});

/// The ceiling of every `gainₖ`'s declared range, applied to every gain the operator reads: the
/// declared range bounds only knob-set values, not a wired signal or a raw OSC float.
const GAIN_MAX: f32 = 4.0;

/// The gain a channel actually applies: 0 for anything not above 0 (NaN included), so a muted
/// channel contributes nothing whatever its source holds, and at most [`GAIN_MAX`].
fn effective_gain(g: f32) -> f32 {
    if g > 0.0 { g.min(GAIN_MAX) } else { 0.0 }
}

const CHANNELS: [(In<SignalF32>, In<SignalF32>); 16] = [
    (IN_IN1, IN_GAIN1),
    (IN_IN2, IN_GAIN2),
    (IN_IN3, IN_GAIN3),
    (IN_IN4, IN_GAIN4),
    (IN_IN5, IN_GAIN5),
    (IN_IN6, IN_GAIN6),
    (IN_IN7, IN_GAIN7),
    (IN_IN8, IN_GAIN8),
    (IN_IN9, IN_GAIN9),
    (IN_IN10, IN_GAIN10),
    (IN_IN11, IN_GAIN11),
    (IN_IN12, IN_GAIN12),
    (IN_IN13, IN_GAIN13),
    (IN_IN14, IN_GAIN14),
    (IN_IN15, IN_GAIN15),
    (IN_IN16, IN_GAIN16),
];

/// One source this block: its samples, its gain samples, and whether the gain is `varying`.
type Live<'a> = (&'a [f32], &'a [f32], bool);

/// Stateless: every output sample is a function of this sample's inputs only.
#[derive(Default)]
pub struct Mix;

impl Mix {
    pub fn new() -> Self {
        Self
    }
}

/// Whether a held input stays at `zero` for the whole block without scanning it: a non-`varying`
/// input is a uniform held fill, so sample 0 stands for every sample. An unwired source, or one
/// parked at gain 0, costs one comparison instead of a pass over the block.
fn held_zero(io: &Io, port: In<SignalF32>, buf: &[f32], zero: impl Fn(f32) -> bool) -> bool {
    !io.varying(port) && buf.first().is_none_or(|&v| zero(v))
}

impl Operator for Mix {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();

        // Gather the live channels before taking the output borrow, into a stack array so nothing
        // allocates. Reads are block-lifetime slices and outlive the `io.write` below.
        let mut live: [Option<Live>; 16] = [None; 16];
        for (slot, &(src, gain)) in live.iter_mut().zip(CHANNELS.iter()) {
            let x = io.read(src);
            let g = io.read(gain);
            if held_zero(io, src, x, |v| v == 0.0)
                || held_zero(io, gain, g, |v| effective_gain(v) == 0.0)
            {
                continue;
            }
            *slot = Some((x, g, io.varying(gain)));
        }

        let out = io.write(OUT_OUT);
        let out = &mut out[..n];
        out.fill(0.0);
        for &(x, g, g_varying) in live.iter().flatten() {
            if g_varying {
                for ((o, &xi), &gi) in out.iter_mut().zip(x).zip(g) {
                    let gi = effective_gain(gi);
                    // A muted sample adds nothing rather than `xi * 0.0`, which is NaN for an
                    // inf or NaN source.
                    *o += if gi > 0.0 { xi * gi } else { 0.0 };
                }
            } else {
                // The gather skips a held gain of effective 0; the guard keeps the mute rule here
                // too, so it does not rest on that skip.
                let gain = g.first().map_or(1.0, |&v| effective_gain(v));
                if gain > 0.0 {
                    for (o, &xi) in out.iter_mut().zip(x) {
                        *o += xi * gain;
                    }
                }
            }
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;
    /// Spans three 128-frame blocks plus a partial one, so every test crosses block boundaries.
    const N: usize = 400;

    fn ramp(scale: f32, offset: f32) -> Vec<f32> {
        (0..N).map(|i| offset + scale * i as f32).collect()
    }

    /// Bit-exact: each oracle sums in port order, the order the operator promises, so a reordered
    /// sum fails here.
    fn exact(a: &[f32], b: &[f32]) {
        assert_eq!(a.len(), b.len());
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "frame {i}: {x} != {y}");
        }
    }

    /// The render block, so a ramp can restart at 0.0 on every block boundary.
    const BLOCK: usize = 128;

    #[test]
    fn contract_is_sixteen_sources_sixteen_gains_one_out() {
        let d = Mix::descriptor();
        assert_eq!(d.type_name, "mix");
        assert_eq!(d.inputs.len(), 32);
        for k in 0..16 {
            assert_eq!(d.inputs[k].name, format!("in{}", k + 1));
            assert_eq!(d.inputs[16 + k].name, format!("gain{}", k + 1));
        }
        assert_eq!(d.outputs.len(), 1);
        assert_eq!(d.outputs[0].name, "out");
        assert!(d.constants.is_empty());
    }

    #[test]
    fn unwired_mix_is_silent() {
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.render(N);
        assert!(d.output(OUT_OUT).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn sums_wired_sources_at_unity_gain_by_default() {
        let (a, b, c) = (ramp(0.001, 0.1), ramp(-0.002, 0.5), ramp(0.0005, -0.3));
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.drive(IN_IN1, &a)
            .drive(IN_IN7, &b)
            .drive(IN_IN16, &c)
            .render(N);
        let want: Vec<f32> = (0..N).map(|i| a[i] + b[i] + c[i]).collect();
        exact(d.output(OUT_OUT), &want);
    }

    #[test]
    fn held_gain_scales_its_own_source_only() {
        let (a, b) = (ramp(0.001, 0.2), ramp(0.003, -0.1));
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.drive(IN_IN2, &a)
            .drive(IN_IN3, &b)
            .set(IN_GAIN2, 0.5)
            .set(IN_GAIN3, 2.0)
            .render(N);
        let want: Vec<f32> = (0..N).map(|i| 0.5 * a[i] + 2.0 * b[i]).collect();
        exact(d.output(OUT_OUT), &want);
    }

    #[test]
    fn swept_gain_applies_per_sample() {
        let a = ramp(0.0, 1.0);
        let g = ramp(1.0 / N as f32, 0.0);
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.drive(IN_IN4, &a).drive(IN_GAIN4, &g).render(N);
        exact(d.output(OUT_OUT), &g);
    }

    #[test]
    fn gain_zero_mutes_a_live_source() {
        let a = ramp(0.001, 0.2);
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.drive(IN_IN5, &a).set(IN_GAIN5, 0.0).render(N);
        assert!(d.output(OUT_OUT).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn a_held_constant_source_is_not_mistaken_for_silence() {
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.set(IN_IN6, 0.25).set(IN_GAIN6, 2.0).render(N);
        exact(d.output(OUT_OUT), &[0.5; N]);
    }

    #[test]
    fn wired_gain_is_clamped_to_its_declared_range() {
        let ones = [1.0f32; N];
        let over = [10.0f32; N];
        let under = [-3.0f32; N];
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.drive(IN_IN1, &ones)
            .drive(IN_GAIN1, &over)
            .drive(IN_IN2, &ones)
            .drive(IN_GAIN2, &under)
            .render(N);
        exact(d.output(OUT_OUT), &[GAIN_MAX; N]);
    }

    #[test]
    fn held_gain_pushed_past_its_range_is_clamped() {
        // A raw OSC float reaches a Signal port unclamped, then reads as a held fill.
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.set(IN_IN1, 1.0).push(IN_GAIN1, 0, 10.0).render(N);
        assert!(d.output(OUT_OUT).iter().all(|&v| v == GAIN_MAX));
    }

    #[test]
    fn a_driven_source_starting_at_zero_is_mixed() {
        // An oscillator at phase 0 or an envelope at rest starts at exactly 0.0.
        let a: Vec<f32> = (0..N).map(|i| (i % BLOCK) as f32 * 0.001).collect();
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.drive(IN_IN8, &a).render(N);
        exact(d.output(OUT_OUT), &a);
    }

    #[test]
    fn every_source_pairs_with_its_own_gain() {
        // By descriptor index, not through `CHANNELS`, so a mispaired table entry fails here.
        for k in 0..16 {
            let g = 0.25 * (k + 1) as f32;
            let mut d = OpDriver::for_type(Mix::new(), SR);
            d.set(k, 1.0).set(16 + k, g).render(N);
            exact(d.output(OUT_OUT), &[g; N]);
        }
    }

    #[test]
    fn a_gain_not_above_zero_mutes_even_a_non_finite_source() {
        let nan = [f32::NAN; N];
        let inf = [f32::INFINITY; N];
        // Held below the range, held NaN, driven at 0, and driven NaN: all mute.
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.drive(IN_IN1, &nan)
            .set(IN_GAIN1, -1.0)
            .drive(IN_IN2, &inf)
            .push(IN_GAIN2, 0, f32::NAN)
            .drive(IN_IN3, &inf)
            .drive(IN_GAIN3, &[0.0; N])
            .set(IN_IN4, 1.0)
            .drive(IN_GAIN4, &nan)
            .render(N);
        assert!(d.output(OUT_OUT).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn spawned_copy_mixes_the_same() {
        let a = ramp(0.001, 0.1);
        let mut d = OpDriver::for_type(Mix::new(), SR);
        d.drive(IN_IN9, &a).set(IN_GAIN9, 3.0).render(N);
        let first = d.output(OUT_OUT).to_vec();
        let mut s = d.spawn();
        s.drive(IN_IN9, &a).set(IN_GAIN9, 3.0).render(N);
        assert_eq!(s.output(OUT_OUT), first.as_slice());
    }
}
