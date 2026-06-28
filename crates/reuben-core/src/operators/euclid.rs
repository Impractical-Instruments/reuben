//! euclid — a clock-driven Euclidean rhythm generator emitting a **sparse gate** (ADR-0030).
//!
//! Walks a Euclidean pattern one step per beat, driven by the [`Clock`](crate::operators::Clock)'s
//! beat `gate`: each rising edge of the `clock` input advances to the next step (wrapping at
//! `steps`) and, **if that step is a pulse**, emits a gate-on; the following falling edge emits the
//! gate-off, so the gate width tracks the clock pulse. Rest steps advance the counter but emit
//! nothing. It is a *rhythm source* — wire its `gate` output into a [`Envelope`](crate::operators::Envelope)
//! `gate` (or any Buffer/`Float` control): the gate is emitted at message rate (one F32 event per
//! edge) and the engine's ZOH bridge (ADR-0030) materializes it into a dense gate buffer for the
//! consumer, so there is no per-sample buffer to fill here.
//!
//! The Euclidean pattern distributes `pulses` active steps as evenly as possible across `steps`
//! total steps — the maximally-even rhythm. Step `s` is a pulse when
//! `((s + rotation) * pulses) mod steps < pulses` (the Bresenham construction, which yields the
//! same family of rhythms as Bjorklund's algorithm and always lands exactly `pulses` hits).
//! `rotation` rotates the ring of hits: at rotation `r`, step `s` plays the base pattern's step
//! `s + r`. E(4,16) is four-on-the-floor (steps 0,4,8,12); E(3,8) is the tresillo (0,3,6).
//!
//! Unified model (ADR-0030): `steps`, `pulses`, and `rotation` are **held `Float` inputs**, each
//! owning its unwired default — read block-rate via [`Io::input`] (rounded to an integer, then
//! clamped/wrapped). `clock` is a **`buffer`** input read per-sample via [`Io::input`] for edge
//! detection. Edge behaviour is forgiving for live knob-twiddling and modulation: `pulses` clamps
//! to `0..=steps` (0 = silence, `steps` = every step hits) and `rotation` wraps modulo `steps`.
//!
//! - input 0: `clock` (`buffer`) — the Clock's beat gate. A rising edge (crossing 0.5 upward)
//!   advances the step and, on a pulse, emits a gate-on; the following falling edge emits the
//!   gate-off. The clock's previous level is held across blocks, so an edge straddling a block
//!   boundary fires exactly once.
//! - input 1: `steps` (`Float`) — total steps in the pattern (default 16).
//! - input 2: `pulses` (`Float`) — active steps to distribute (default 4).
//! - input 3: `rotation` (`Float`) — rotation offset in steps (default 0).
//! - output 0: `gate` (`Float`) — a sparse gate: F32 `1.0` on a pulse step's rising clock edge,
//!   F32 `0.0` on the following falling edge.
//!
//! Single-Lane by design: the gate is emitted pre-fan-out, a mono trigger line (Lane 0 only).

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

/// Maximum number of steps in the pattern — the `steps` input clamps to this.
pub const NUM_STEPS: usize = 16;

// Single-source contract (ADR-0025/0030): one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Euclid {
    type_name: "euclid",
    inputs:  { clock:    f32 { 0.0..=1.0, default 0.0, "", lin },
               steps:    f32 { 1.0..=16.0, default 16.0, "steps",  lin },
               pulses:   f32 { 0.0..=16.0, default 4.0,  "pulses", lin },
               rotation: f32 { 0.0..=15.0, default 0.0,  "steps",  lin } },
    outputs: { gate: f32 { 0.0..=1.0, default 0.0, "gate", lin } },
});

/// Whether step `step` (0-based, already in `0..total`) is a Euclidean pulse for the
/// `pulses`-of-`total` pattern rotated by `rotation`. The Bresenham construction
/// `((s + rotation) * pulses) mod total < pulses` lands exactly `pulses` hits, spread maximally
/// evenly; the `pulses == 0` / `pulses >= total` ends are the trivial silent / every-step patterns.
fn is_pulse(step: i64, total: i64, pulses: i64, rotation: i64) -> bool {
    if pulses <= 0 {
        return false;
    }
    if pulses >= total {
        return true;
    }
    let i = (step + rotation).rem_euclid(total);
    (i * pulses).rem_euclid(total) < pulses
}

pub struct Euclid {
    /// Index of the current step, or -1 before the first beat edge. Continuous across blocks.
    /// Advanced (and wrapped at `steps`) on each rising edge of the clock input.
    step: i64,
    /// Clock input level at the previous sample, so a rising/falling edge is detected across the
    /// block boundary (and a clock that starts already-high fires its first edge at 0).
    prev_clock: f32,
    /// Whether a gate-on has been emitted and not yet closed by a gate-off (the per-step pulse).
    high: bool,
}

impl Default for Euclid {
    fn default() -> Self {
        Self {
            step: -1,
            prev_clock: 0.0,
            high: false,
        }
    }
}

impl Euclid {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Euclid {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        // Held controls, constant for this (sub)block (the engine block-slices at changes).
        let total =
            (io.input::<f32>(IN_STEPS).unwrap_or(16.0).round() as i64).clamp(1, NUM_STEPS as i64);
        let pulses = (io.input::<f32>(IN_PULSES).unwrap_or(4.0).round() as i64).clamp(0, total);
        let rotation =
            (io.input::<f32>(IN_ROTATION).unwrap_or(0.0).round() as i64).rem_euclid(total);

        // `clock` is a held Value (ADR-0031): the engine block-slices at every clock change, so this
        // call sees one constant level. Compare it to the level held across the previous slice to
        // detect the edge; the slice's frame 0 *is* the change frame (block-absolute), so emitting
        // there is sample-accurate. The held latch carries `prev` across blocks/slices.
        let g = io.input::<f32>(IN_CLOCK).unwrap_or(0.0);
        let prev = self.prev_clock;
        if prev < 0.5 && g >= 0.5 {
            // Rising edge: close any open gate, advance, and open a gate on a pulse step.
            if self.high {
                io.output::<f32>(OUT_GATE).set(0, 0.0f32);
                self.high = false;
            }
            self.step = (self.step + 1).rem_euclid(total);
            if is_pulse(self.step, total, pulses, rotation) {
                io.output::<f32>(OUT_GATE).set(0, 1.0f32);
                self.high = true;
            }
        } else if prev >= 0.5 && g < 0.5 {
            // Falling edge: close the gate so its width tracks the clock pulse.
            if self.high {
                io.output::<f32>(OUT_GATE).set(0, 0.0f32);
                self.high = false;
            }
        }
        self.prev_clock = g;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Euclid);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Emit};
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Drive a fresh Euclid with a `clock` beat gate and the `steps`/`pulses`/`rotation` held
    /// controls through the real engine. Returns the emitted gate Messages (block-absolute frames).
    fn run(clock: &[f32], steps: f32, pulses: f32, rotation: f32) -> Vec<Emit> {
        let mut d = OpDriver::for_type(Euclid::new(), SR);
        d.set(IN_STEPS, steps)
            .set(IN_PULSES, pulses)
            .set(IN_ROTATION, rotation);
        push_clock(&mut d, clock, 0.0);
        d.render(clock.len()).emits().to_vec()
    }

    /// Drive the now-held-Value `clock` from a dense gate buffer (ADR-0031): push a held-level
    /// change at each frame the buffer crosses the 0.5 threshold — the clock is fed by edges, not a
    /// per-sample buffer. `prev` threads the level across split renders; returns the trailing level.
    fn push_clock(d: &mut OpDriver, clock: &[f32], mut prev: f32) -> f32 {
        for (i, &g) in clock.iter().enumerate() {
            if (prev < 0.5) != (g < 0.5) {
                d.push(IN_CLOCK, i, g);
                prev = g;
            }
        }
        prev
    }

    /// A clock gate: high for the first half of each `period`-sample beat, repeated `beats` times.
    fn beat_gate(period: usize, beats: usize) -> Vec<f32> {
        let mut g = Vec::with_capacity(period * beats);
        for _ in 0..beats {
            for i in 0..period {
                g.push(if i < period / 2 { 1.0 } else { 0.0 });
            }
        }
        g
    }

    /// The F32 gate value carried by an emit (panics on any other Arg — the contract is F32).
    fn gate(e: &Emit) -> f32 {
        match &e.arg {
            Arg::F32(v) => *v,
            other => panic!("expected an F32 gate, got {other:?}"),
        }
    }

    /// The 0-based step (beat) indices at which a gate-on (value 1.0) was emitted, given the
    /// fixed beat `period` — each beat is exactly one step, so `frame / period` is the step.
    fn on_steps(emits: &[Emit], period: usize) -> Vec<usize> {
        emits
            .iter()
            .filter(|e| gate(e) >= 0.5)
            .map(|e| e.frame / period)
            .collect()
    }

    #[test]
    fn default_pattern_is_four_on_the_floor() {
        // E(4,16): four pulses spread evenly over 16 steps -> hits on steps 0, 4, 8, 12.
        let period = 64;
        let clock = beat_gate(period, NUM_STEPS);
        let emits = run(&clock, 16.0, 4.0, 0.0);
        assert_eq!(on_steps(&emits, period), vec![0, 4, 8, 12]);
    }

    #[test]
    fn tresillo_e3_8() {
        // E(3,8): the classic tresillo x..x..x. -> hits on steps 0, 3, 6.
        let period = 100;
        let clock = beat_gate(period, 8);
        let emits = run(&clock, 8.0, 3.0, 0.0);
        assert_eq!(on_steps(&emits, period), vec![0, 3, 6]);
    }

    #[test]
    fn rotation_rotates_the_ring_of_hits() {
        // E(3,8) rotated by 2 plays the base pattern's step s+2 at step s -> hits on 1, 4, 6.
        let period = 100;
        let clock = beat_gate(period, 8);
        let emits = run(&clock, 8.0, 3.0, 2.0);
        assert_eq!(on_steps(&emits, period), vec![1, 4, 6]);
    }

    #[test]
    fn rotation_wraps_modulo_steps() {
        // Rotation 8 on an 8-step pattern wraps to 0 -> identical to the unrotated tresillo.
        let period = 100;
        let clock = beat_gate(period, 8);
        let r0 = on_steps(&run(&clock, 8.0, 3.0, 0.0), period);
        let r8 = on_steps(&run(&clock, 8.0, 3.0, 8.0), period);
        assert_eq!(r0, r8);
    }

    #[test]
    fn zero_pulses_is_silent() {
        // pulses 0 -> no step is a hit, so nothing is ever emitted.
        let period = 100;
        let clock = beat_gate(period, 8);
        let emits = run(&clock, 8.0, 0.0, 0.0);
        assert!(emits.is_empty(), "no gates for a zero-pulse pattern");
    }

    #[test]
    fn pulses_equal_to_steps_hits_every_step() {
        // pulses == steps -> every step is a hit: one gate-on per beat.
        let period = 64;
        let clock = beat_gate(period, NUM_STEPS);
        let emits = run(&clock, 16.0, 16.0, 0.0);
        assert_eq!(on_steps(&emits, period), (0..NUM_STEPS).collect::<Vec<_>>());
    }

    #[test]
    fn pulses_clamp_to_steps() {
        // pulses 99 clamps to steps -> every step hits, same as pulses == steps (no panic/wrap).
        let period = 64;
        let clock = beat_gate(period, 8);
        let emits = run(&clock, 8.0, 99.0, 0.0);
        assert_eq!(on_steps(&emits, period), (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn gate_rises_on_the_pulse_and_falls_at_the_clock_fall() {
        // A single-step pattern that always hits: gate-on at the downbeat (frame 0), gate-off at
        // the clock's falling edge (period/2) — the gate width tracks the clock pulse.
        let period = 100;
        let clock = beat_gate(period, 1);
        let emits = run(&clock, 1.0, 1.0, 0.0);
        assert_eq!(emits.len(), 2, "one gate-on + one gate-off");
        assert_eq!(emits[0].frame, 0);
        approx::assert_relative_eq!(gate(&emits[0]), 1.0);
        assert_eq!(emits[1].frame, period / 2);
        approx::assert_relative_eq!(gate(&emits[1]), 0.0);
    }

    #[test]
    fn rest_step_emits_nothing_between_neighbouring_hits() {
        // E(1,2): hit on step 0, rest on step 1. Over two beats: on+off for beat 0, silence beat 1.
        let period = 100;
        let clock = beat_gate(period, 2);
        let emits = run(&clock, 2.0, 1.0, 0.0);
        assert_eq!(emits.len(), 2, "on+off for the single hit only");
        assert!(emits.iter().all(|e| e.frame < period), "all within beat 0");
    }

    #[test]
    fn pattern_wraps_after_steps_beats() {
        // E(3,8) over 16 beats repeats the {0,3,6} pattern across both 8-step cycles.
        let period = 64;
        let clock = beat_gate(period, 16);
        let emits = run(&clock, 8.0, 3.0, 0.0);
        assert_eq!(on_steps(&emits, period), vec![0, 3, 6, 8, 11, 14]);
    }

    #[test]
    fn gate_state_is_continuous_across_calls() {
        // Splitting the clock across two renders yields the same gate-on steps as one whole render:
        // the step machine, edge detection, and held clock level thread across the boundary.
        let period = 64;
        let clock = beat_gate(period, NUM_STEPS);
        let whole = on_steps(&run(&clock, 16.0, 5.0, 1.0), period);

        let mid = clock.len() / 2;
        let mut split = OpDriver::for_type(Euclid::new(), SR);
        split
            .set(IN_STEPS, 16.0)
            .set(IN_PULSES, 5.0)
            .set(IN_ROTATION, 1.0);
        let prev = push_clock(&mut split, &clock[..mid], 0.0);
        let e1 = split.render(mid).emits().to_vec();
        push_clock(&mut split, &clock[mid..], prev);
        let e2 = split.render(clock.len() - mid).emits().to_vec();

        // Re-base the second block's on-steps onto the whole-clock timeline before comparing.
        let mut ons: Vec<usize> = on_steps(&e1, period);
        ons.extend(on_steps(&e2, period).into_iter().map(|s| s + mid / period));
        assert_eq!(ons, whole);
    }

    #[test]
    fn spawned_euclid_starts_before_the_first_step() {
        // A fresh spawn replays from step 0, not where the parent left off.
        let period = 64;
        let mut a = OpDriver::for_type(Euclid::new(), SR);
        a.set(IN_STEPS, 8.0)
            .set(IN_PULSES, 3.0)
            .set(IN_ROTATION, 0.0);
        let clock = beat_gate(period, 8);
        push_clock(&mut a, &clock, 0.0);
        a.render(clock.len());

        let mut b = a.spawn();
        b.set(IN_STEPS, 8.0)
            .set(IN_PULSES, 3.0)
            .set(IN_ROTATION, 0.0);
        let one = beat_gate(period, 1);
        push_clock(&mut b, &one, 0.0);
        let emits = b.render(one.len()).emits().to_vec();
        // The spawn's first beat is step 0, a pulse in E(3,8): gate-on at frame 0.
        let first_on = emits.iter().find(|e| gate(e) >= 0.5).expect("a gate-on");
        assert_eq!(first_on.frame, 0);
    }

    #[test]
    fn descriptor_defaults_match_the_frozen_contract() {
        // The contract freeze the rig builder wires against: default E(4,16), single Float gate out.
        let desc = Euclid::descriptor();
        assert_eq!(desc.inputs[IN_STEPS].meta.as_ref().unwrap().default, 16.0);
        assert_eq!(desc.inputs[IN_PULSES].meta.as_ref().unwrap().default, 4.0);
        assert_eq!(desc.inputs[IN_ROTATION].meta.as_ref().unwrap().default, 0.0);
        assert_eq!(desc.outputs.len(), 1, "one gate output");
    }
}
