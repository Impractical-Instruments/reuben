//! `m2s` — the one sanctioned Message→Signal bridge (ADR-0017).
//!
//! Control is Message-first; CV is the opt-in special case. Crossing from the Message domain
//! to a Signal is **always an explicit operator**, because the crossing needs an *authored
//! policy*: how do you fill the dense per-sample gaps between sparse messages? That policy is
//! the `mode` param, and it lives **here, once** — never re-implemented in every operator that
//! could take either carrier (the reason cutoff/freq/etc. are Signal-only, ADR-0017).
//!
//! - input 0: `in` (Message) — value events (any address; first numeric arg is the target).
//! - output 0: `out` (Signal) — the materialized per-sample control signal.
//! - param 0: `mode` — 0 = snap, 1 = slew, 2 = smooth, 3 = glide.
//! - param 1: `rate` — slew rate in units/second.
//! - param 2: `time` — time constant in seconds (smooth) or ramp time (glide).
//! - param 3: `default` — value held before the first message arrives (and the unwired
//!   resting value, so a Good Button has a sensible tone at load).
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

use crate::descriptor::Descriptor;
use crate::message::Arg;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025): one declaration -> IN_/OUT_/P_ consts + Descriptor, no drift.
crate::operator_contract!(M2s {
    inputs:  { in: message },
    outputs: { out: signal },
    params:  { mode:    { 0.0..=3.0,                  default 2.0,     "",   lin },
               rate:    { 0.0..=1_000_000.0,          default 1_000.0, "/s", exp },
               time:    { 0.0..=10.0,                 default 0.05,    "s",  exp },
               default: { -1_000_000.0..=1_000_000.0, default 0.0,     "",   lin } },
});

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
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sr = io.sample_rate();
        let mode = io.param(P_MODE).round() as i32;
        let rate = io.param(P_RATE).max(0.0);
        let time = io.param(P_TIME).max(0.0);
        let default = io.param(P_DEFAULT);

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

    /// Run the converter over one block with the given params and value events; returns `out`.
    fn run(params: &[f32], values: &[Message], n: usize, state: &mut M2s) -> Vec<f32> {
        let evs: Vec<Event> = values
            .iter()
            .map(|m| Event {
                addr: &m.addr,
                args: &m.args,
                frame: m.frame,
            })
            .collect();
        let mut out = vec![0.0f32; n];
        {
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, n, inputs, outs, params, &evs);
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
        let params = [0.0, 1000.0, 0.05, 4000.0];
        let out = run(&params, &[], 64, &mut M2s::new());
        assert!(out.iter().all(|&s| (s - 4000.0).abs() < 1e-3));
    }

    #[test]
    fn snap_steps_sample_accurately() {
        let params = [0.0, 1000.0, 0.05, 0.0]; // snap, default 0
        let out = run(&params, &[val(1.0, 32)], 64, &mut M2s::new());
        assert!(out[..32].iter().all(|&s| s == 0.0));
        assert!(out[32..].iter().all(|&s| s == 1.0));
    }

    #[test]
    fn slew_is_rate_limited() {
        // rate = 48000 units/s @ 48k => 1.0 unit/sample. Target 10 from 0 reaches it in 10
        // samples, not instantly.
        let params = [1.0, 48_000.0, 0.05, 0.0];
        let out = run(&params, &[val(10.0, 0)], 64, &mut M2s::new());
        approx::assert_relative_eq!(out[0], 1.0, epsilon = 1e-4); // one step
        approx::assert_relative_eq!(out[9], 10.0, epsilon = 1e-4); // reached
        assert!(out[20..].iter().all(|&s| (s - 10.0).abs() < 1e-4));
    }

    #[test]
    fn smooth_approaches_monotonically_without_overshoot() {
        let params = [2.0, 1000.0, 0.01, 0.0]; // smooth, 10ms
        let out = run(&params, &[val(1.0, 0)], 2048, &mut M2s::new());
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
        let params = [3.0, 1000.0, time, 0.0];
        let out = run(&params, &[val(64.0, 0)], 128, &mut M2s::new());
        // Linear ramp: around the midpoint the value is ~halfway.
        approx::assert_relative_eq!(out[31], 32.0, epsilon = 1.5);
        assert!(out[64..].iter().all(|&s| (s - 64.0).abs() < 1e-3));
    }

    #[test]
    fn value_carries_across_blocks() {
        // smooth: the partially-approached value at block end resumes next block.
        let params = [2.0, 1000.0, 0.05, 0.0];
        let mut m = M2s::new();
        let b1 = run(&params, &[val(1.0, 0)], 64, &mut m);
        let b2 = run(&params, &[], 64, &mut m);
        assert!(
            b2[0] >= b1[63] - 1e-6,
            "must continue approaching, not reset"
        );
        assert!(b2[63] > b1[63], "keeps rising across the boundary");
    }

    #[test]
    fn spawned_converter_starts_uninitialized() {
        let params = [0.0, 1000.0, 0.05, 7.0];
        let mut m = M2s::new();
        let _ = run(&params, &[val(1.0, 0)], 64, &mut m);
        let mut m2 = m.spawn();
        // Fresh spawn re-seeds from `default` (7.0), not where `m` ended (1.0).
        let evs: Vec<Event> = Vec::new();
        let mut out = [0.0f32; 8];
        {
            let inputs: Vec<Option<&[f32]>> = vec![None];
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let mut io = Io::new(SR, 8, inputs, outs, &params, &evs);
            m2.process(&mut io);
        }
        assert!(out.iter().all(|&s| (s - 7.0).abs() < 1e-3));
    }
}
