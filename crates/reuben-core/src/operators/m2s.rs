//! `m2s` — the one sanctioned Message→Signal bridge (ADR-0017).
//!
//! Control is Message-first; CV is the opt-in special case. Crossing from the Message domain
//! to a Signal is **always an explicit operator**, because the crossing needs an *authored
//! policy*: how do you fill the dense per-sample gaps between sparse messages? That policy is
//! the `mode` param, and it lives **here, once** — never re-implemented in every operator that
//! could take either carrier (the reason cutoff/freq/etc. are Signal-only, ADR-0017).
//!
//! - input 0: `in` (Message) — value events (any address; first numeric arg is the target).
//! - input 1: `mode` (`Enum` {Snap, Slew, Smooth, Glide}) — the fill policy; default `Smooth`.
//! - input 2: `rate` (`Float`) — slew rate in units/second.
//! - input 3: `time` (`Float`) — time constant in seconds (smooth) or ramp time (glide).
//! - input 4: `default` (`Float`) — value held before the first message arrives (and the unwired
//!   resting value, so a Good Button has a sensible tone at load).
//! - output 0: `out` (`Float`) — the materialized per-sample control signal.
//!
//! ADR-0028: this stays the Message→`Float` bridge for now; its split into `Float→Float`
//! `slew`/`glide`/`smooth` shaper ops (materialize makes `snap` the engine default) lands with the
//! instrument migration. Hand-written so `mode`'s default is `Smooth` (the macro enum defaults to
//! the first variant; reordering would change the on-wire indices instruments already use).
//!
//! Modes:
//! - **snap** — step to the target at the message frame (sample-accurate; what param
//!   block-slicing already does, now materialized as a Signal).
//! - **slew** — rate-limited linear approach (`rate` units/s).
//! - **smooth** — one-pole exponential approach (`time`); the natural knob feel.
//! - **glide** — fixed-time linear ramp to the target (`time`); portamento, retargeting per
//!   message.
//!
//! True linear interpolation *between* messages is excluded — it needs the next message, so it
//! is not RT-causal without a one-block delay (ADR-0017). Signal→Message (a sampling policy)
//! is deferred. Single-input: consumes *any* value event, so it composes with both external
//! OSC (to the node address) and an upstream emit. State (current value, glide ramp) carries
//! across blocks.

use crate::descriptor::{Curve, Descriptor, EnumMeta, LaneRule, ParamMeta, Port};
use crate::message::Arg;
use crate::operator::{Io, Operator};

/// `in` Message input (value events via [`Io::events`]).
pub const IN_IN: usize = 0;
/// `mode` `Enum` input — the fill policy {Snap, Slew, Smooth, Glide}.
pub const IN_MODE: usize = 1;
/// `rate`/`time`/`default` `Float` inputs (read block-rate).
pub const IN_RATE: usize = 2;
pub const IN_TIME: usize = 3;
pub const IN_DEFAULT: usize = 4;
/// `out` `Float` output.
pub const OUT_OUT: usize = 0;

/// `mode` variant symbols (index-aligned with the historic numeric mode: 0..3).
const MODES: &[&str] = &["Snap", "Slew", "Smooth", "Glide"];
const MODE_SNAP: i32 = 0;
const MODE_SLEW: i32 = 1;
const MODE_SMOOTH: i32 = 2;
const MODE_GLIDE: i32 = 3;

#[derive(Default)]
pub struct M2s {
    /// Current output value, held across blocks.
    cur: f32,
    /// Target the current value is approaching (the last message value).
    target: f32,
    /// Glide ramp: per-sample increment and remaining samples.
    glide_inc: f32,
    glide_left: u32,
    /// Whether `cur`/`target` have been seeded from the `default` param yet.
    initialized: bool,
}

impl M2s {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for M2s {
    fn descriptor() -> Descriptor {
        let range = |name: &'static str, min, max, default, unit, curve| {
            Port::float(ParamMeta {
                name,
                min,
                max,
                default,
                unit,
                curve,
            })
        };
        Descriptor {
            type_name: "m2s",
            inputs: vec![
                Port::message("in"),
                // `mode` defaults to `Smooth` (index 2) — the natural knob feel and the prior
                // param default; variant order is the on-wire index instruments already use.
                Port::enumerated(EnumMeta {
                    name: "mode",
                    variants: MODES,
                    default: MODE_SMOOTH as usize,
                }),
                range("rate", 0.0, 1_000_000.0, 1_000.0, "/s", Curve::Exponential),
                range("time", 0.0, 10.0, 0.05, "s", Curve::Exponential),
                range("default", -1_000_000.0, 1_000_000.0, 0.0, "", Curve::Linear),
            ],
            outputs: vec![Port::signal("out")],
            params: vec![],
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sr = io.sample_rate();
        let mode = io.enum_index(IN_MODE) as i32;
        let rate = io.value(IN_RATE).max(0.0);
        let time = io.value(IN_TIME).max(0.0);
        let default = io.value(IN_DEFAULT);

        if !self.initialized {
            self.cur = default;
            self.target = default;
            self.initialized = true;
        }

        // Snapshot value events (can't read events while writing the output buffer), sorted.
        let mut events: smallvec::SmallVec<[(usize, f32); 8]> = smallvec::SmallVec::new();
        for ev in io.events() {
            if let Some(v) = ev.args.first().and_then(Arg::as_f32) {
                events.push((ev.frame.min(n), v));
            }
        }
        events.sort_by_key(|(f, _)| *f);

        // Per-sample smoothing coefficient for the one-pole (smooth mode).
        let tau_samples = (time * sr).max(1e-6);
        let smooth_coeff = 1.0 - (-1.0 / tau_samples).exp();
        let slew_step = if sr > 0.0 { rate / sr } else { 0.0 };

        let mut cur = self.cur;
        let mut target = self.target;
        let mut glide_inc = self.glide_inc;
        let mut glide_left = self.glide_left;

        let mut ei = 0usize;
        let out = io.output(OUT_OUT);
        for (i, slot) in out[..n].iter_mut().enumerate() {
            // Apply every event landing at this sample (last wins).
            while ei < events.len() && events[ei].0 == i {
                let v = events[ei].1;
                target = v;
                match mode {
                    MODE_SNAP => cur = v,
                    MODE_GLIDE => {
                        let total = (time * sr).round().max(1.0);
                        glide_inc = (target - cur) / total;
                        glide_left = total as u32;
                    }
                    _ => {}
                }
                ei += 1;
            }

            // Advance the value toward the target per the mode's policy.
            match mode {
                MODE_SNAP => {}
                MODE_SLEW => {
                    if cur < target {
                        cur = (cur + slew_step).min(target);
                    } else {
                        cur = (cur - slew_step).max(target);
                    }
                }
                MODE_SMOOTH => cur += (target - cur) * smooth_coeff,
                MODE_GLIDE => {
                    if glide_left > 0 {
                        cur += glide_inc;
                        glide_left -= 1;
                    } else {
                        cur = target;
                    }
                }
                _ => cur = target,
            }
            *slot = cur;
        }

        self.cur = cur;
        self.target = target;
        self.glide_inc = glide_inc;
        self.glide_left = glide_left;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(M2s);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Event, Message};

    const SR: f32 = 48_000.0;

    /// Run the converter over one block: `mode` is the held `Enum` index, rate/time/default the
    /// `Float` inputs (constant buffers, the way the engine materializes them, ADR-0028), plus the
    /// value events on `in`; returns `out`.
    fn run(
        mode: usize,
        rate: f32,
        time: f32,
        default: f32,
        values: &[Message],
        n: usize,
        state: &mut M2s,
    ) -> Vec<f32> {
        let evs: Vec<Event> = values
            .iter()
            .map(|m| Event {
                addr: &m.addr,
                args: &m.args,
                frame: m.frame,
            })
            .collect();
        let (rate_buf, time_buf, def_buf) = (vec![rate; n], vec![time; n], vec![default; n]);
        let enums = [0usize, mode, 0, 0, 0]; // held index at IN_MODE = 1
        let mut out = vec![0.0f32; n];
        {
            // Port order: in (Message), mode (Enum), rate, time, default.
            let inputs: Vec<Option<&[f32]>> = vec![
                None,
                None,
                Some(&rate_buf[..]),
                Some(&time_buf[..]),
                Some(&def_buf[..]),
            ];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, n, inputs, outs, &[], &evs).with_enums(&enums);
            state.process(&mut io);
        }
        out
    }

    fn val(v: f32, frame: usize) -> Message {
        Message::new("in", [Arg::Float(v)], frame)
    }

    #[test]
    fn default_is_held_before_any_message() {
        // mode=snap, default=4000: the whole block reads 4000 with no events.
        let out = run(0, 1000.0, 0.05, 4000.0, &[], 64, &mut M2s::new());
        assert!(out.iter().all(|&s| (s - 4000.0).abs() < 1e-3));
    }

    #[test]
    fn snap_steps_sample_accurately() {
        let out = run(0, 1000.0, 0.05, 0.0, &[val(1.0, 32)], 64, &mut M2s::new()); // snap
        assert!(out[..32].iter().all(|&s| s == 0.0));
        assert!(out[32..].iter().all(|&s| s == 1.0));
    }

    #[test]
    fn slew_is_rate_limited() {
        // rate = 48000 units/s @ 48k => 1.0 unit/sample. Target 10 from 0 reaches it in 10
        // samples, not instantly.
        let out = run(1, 48_000.0, 0.05, 0.0, &[val(10.0, 0)], 64, &mut M2s::new()); // slew
        approx::assert_relative_eq!(out[0], 1.0, epsilon = 1e-4); // one step
        approx::assert_relative_eq!(out[9], 10.0, epsilon = 1e-4); // reached
        assert!(out[20..].iter().all(|&s| (s - 10.0).abs() < 1e-4));
    }

    #[test]
    fn smooth_approaches_monotonically_without_overshoot() {
        let out = run(2, 1000.0, 0.01, 0.0, &[val(1.0, 0)], 2048, &mut M2s::new()); // smooth, 10ms
                                                                                    // Rises toward 1.0, never past it, and gets most of the way there.
        for w in out.windows(2) {
            assert!(w[1] >= w[0] - 1e-6, "smooth must not decrease");
            assert!(w[1] <= 1.0 + 1e-6, "smooth must not overshoot");
        }
        assert!(out[out.len() - 1] > 0.9, "should approach the target");
    }

    #[test]
    fn glide_ramps_linearly_over_time() {
        // glide, time = 64/48000 s => exactly 64-sample ramp from 0 to 64.
        let time = 64.0 / SR;
        let out = run(3, 1000.0, time, 0.0, &[val(64.0, 0)], 128, &mut M2s::new()); // glide
                                                                                    // Linear ramp: around the midpoint the value is ~halfway.
        approx::assert_relative_eq!(out[31], 32.0, epsilon = 1.5);
        assert!(out[64..].iter().all(|&s| (s - 64.0).abs() < 1e-3));
    }

    #[test]
    fn value_carries_across_blocks() {
        // smooth: the partially-approached value at block end resumes next block.
        let mut m = M2s::new();
        let b1 = run(2, 1000.0, 0.05, 0.0, &[val(1.0, 0)], 64, &mut m); // smooth
        let b2 = run(2, 1000.0, 0.05, 0.0, &[], 64, &mut m);
        assert!(
            b2[0] >= b1[63] - 1e-6,
            "must continue approaching, not reset"
        );
        assert!(b2[63] > b1[63], "keeps rising across the boundary");
    }

    #[test]
    fn spawned_converter_starts_uninitialized() {
        let mut m = M2s::new();
        let _ = run(0, 1000.0, 0.05, 7.0, &[val(1.0, 0)], 64, &mut m); // snap, default 7
        let mut m2 = m.spawn();
        // Fresh spawn re-seeds from `default` (7.0), not where `m` ended (1.0).
        let (rate, time, def) = (vec![1000.0f32; 8], vec![0.05f32; 8], vec![7.0f32; 8]);
        let enums = [0usize, 0, 0, 0, 0]; // mode Snap (index 0)
        let mut out = [0.0f32; 8];
        {
            let inputs: Vec<Option<&[f32]>> =
                vec![None, None, Some(&rate[..]), Some(&time[..]), Some(&def[..])];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, 8, inputs, outs, &[], &[]).with_enums(&enums);
            m2.process(&mut io);
        }
        assert!(out.iter().all(|&s| (s - 7.0).abs() < 1e-3));
    }
}
