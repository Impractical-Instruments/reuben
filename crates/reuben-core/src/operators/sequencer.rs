//! Sequencer — a clock-driven step sequencer that emits note Messages (V1.1, ADR-0014; V1.3
//! gate-mode + 16 steps, ADR-0022; unified model, ADR-0030).
//!
//! Walks a fixed pattern, one step per beat, driven by the [`Clock`]'s beat `gate`: each rising
//! edge of the clock input advances to the next step (wrapping at `length`) and **emits a degree
//! [`Note`]** for that step; the note is released when the beat gate falls. It is a note *source*
//! on the internal message graph — wire its `degrees` output to a
//! [`Voicer`](crate::operators::Voicer) (`sequencer.degrees → voicer.notes`) and the sequence is
//! polyphony-, transpose-, and snap-composable, exactly like notes arriving from outside.
//!
//! Unified model (ADR-0030): `length`, `step1`..`step16`, and `pitch` are **held `Float` inputs**,
//! each owning its unwired default — read via [`Io::input`]. `gate_mode` is a held **`enum`** input
//! [`GateMode`] {`Degree`, `Gate`}. `clock` is a **`buffer`** input read per-sample via
//! [`Io::input`] for edge detection.
//!
//! Two step interpretations, selected by `gate_mode` (ADR-0022):
//! - **degree mode** (`Degree`, default): each `stepN` *is* the scale degree to play that beat; a
//!   value below 0 is a rest.
//! - **gate mode** (`Gate`): each `stepN` reads as a **boolean on/off** (≥ 0.5 = hit) and every
//!   hit emits the single `pitch` degree. This is the groove-box step grid (ADR-0022).
//!
//! - input 0: `clock` (`buffer`) — the Clock's beat gate. A rising edge (crossing 0.5 upward)
//!   advances the step and emits a note-on; the following falling edge emits the note-off. The
//!   clock's previous level is held across blocks, so an edge straddling a block boundary fires
//!   exactly once.
//! - inputs 1..=17: `length` (active steps 1..=16; default 8) then `step1`..`step16` (per-step
//!   value — a degree in degree mode, a boolean hit in gate mode).
//! - input 18: `gate_mode` (`enum` {Degree, Gate}) — `Degree` is the default.
//! - input 19: `pitch` (`Float`) — the degree emitted on each hit in gate mode (default 0 = root).
//! - output 0: `degrees` (`Note`) — a degree note (velocity 1 = on, 0 = off). The Voicer resolves
//!   the degree through the tonal context.
//!
//! Emits one mono note line, upstream of the downstream Voicer that fans it out to voices (ADR-0032).

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};
use crate::operators::edge::{Edge, EdgeDetector};
use crate::vocab::pitch::{Note, Pitch};
use crate::vocab::GateMode;

/// Number of step slots in the pattern (V1.3: expanded 8 → 16, ADR-0022).
pub const NUM_STEPS: usize = 16;

// Single-source contract (ADR-0025/0030). `gate_mode` references the shared `GateMode` vocab enum.
crate::operator_contract!(Sequencer {
    inputs:  { clock:  f32 { 0.0..=1.0, default 0.0, "", lin },
               length: f32 { 1.0..=16.0, default 8.0, "steps", lin },
               step1:  f32 { -1.0..=24.0, default 0.0, "degree", lin },
               step2:  f32 { -1.0..=24.0, default 1.0, "degree", lin },
               step3:  f32 { -1.0..=24.0, default 2.0, "degree", lin },
               step4:  f32 { -1.0..=24.0, default 3.0, "degree", lin },
               step5:  f32 { -1.0..=24.0, default 4.0, "degree", lin },
               step6:  f32 { -1.0..=24.0, default 5.0, "degree", lin },
               step7:  f32 { -1.0..=24.0, default 6.0, "degree", lin },
               step8:  f32 { -1.0..=24.0, default 7.0, "degree", lin },
               step9:  f32 { -1.0..=24.0, default 0.0, "degree", lin },
               step10: f32 { -1.0..=24.0, default 0.0, "degree", lin },
               step11: f32 { -1.0..=24.0, default 0.0, "degree", lin },
               step12: f32 { -1.0..=24.0, default 0.0, "degree", lin },
               step13: f32 { -1.0..=24.0, default 0.0, "degree", lin },
               step14: f32 { -1.0..=24.0, default 0.0, "degree", lin },
               step15: f32 { -1.0..=24.0, default 0.0, "degree", lin },
               step16: f32 { -1.0..=24.0, default 0.0, "degree", lin },
               gate_mode: enum(GateMode),
               pitch:  f32 { -1.0..=24.0, default 0.0, "degree", lin } },
    outputs: { degrees: note },
});

/// Message output ordinal of the `degrees` port (the index [`Io::output`] uses).
pub const MSG_NOTES: usize = OUT_DEGREES;

/// Per-sample value of step `k` (0-based): input `IN_STEP1 + k`.
const fn in_step(k: usize) -> usize {
    IN_STEP1 + k
}

pub struct Sequencer {
    /// Index of the current step, or -1 before the first beat edge. Continuous across
    /// blocks. Advanced (and wrapped at `length`) on each rising edge of the clock input.
    step: i64,
    /// Detects the rising/falling edge of the clock input across the block boundary (and a clock
    /// that starts already-high fires its first edge at 0). See [`EdgeDetector`].
    clock: EdgeDetector,
    /// Scale degree currently sounding (emitted note-on, not yet note-off), for release.
    held: Option<f32>,
}

impl Default for Sequencer {
    fn default() -> Self {
        Self {
            step: -1,
            clock: EdgeDetector::new(),
            held: None,
        }
    }
}

impl Sequencer {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A degree note from a (possibly fractional) degree value, rounded to the nearest scale degree.
fn degree_note(degree: f32, velocity: f32) -> Note {
    Note::new(Pitch::Degree(degree.round() as i32), velocity)
}

impl Operator for Sequencer {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let length =
            (io.input::<f32>(IN_LENGTH).unwrap_or(8.0).round() as i64).clamp(1, NUM_STEPS as i64);
        let gate_mode = io.input::<GateMode>(IN_GATE_MODE).unwrap_or_default() == GateMode::Gate;
        let pitch = io.input::<f32>(IN_PITCH).unwrap_or(0.0);

        // Snapshot the held step values: constant for this (sub)block.
        let mut steps = [0.0f32; NUM_STEPS];
        for (k, p) in steps.iter_mut().enumerate() {
            *p = io.input::<f32>(in_step(k)).unwrap_or(0.0);
        }
        // The degree to emit for a given step, or None for a rest / off-step.
        // - degree mode: the step value IS the degree; below 0 is a rest.
        // - gate mode: the step is a boolean hit (≥ 0.5); a hit emits the `pitch` degree,
        //   itself a rest if `pitch` < 0.
        let note_at = |step: i64| -> Option<f32> {
            if step < 0 {
                return None;
            }
            let v = steps[(step as usize) % NUM_STEPS];
            if gate_mode {
                (v >= 0.5 && pitch >= 0.0).then_some(pitch)
            } else {
                (v >= 0.0).then_some(v)
            }
        };

        // `clock` is a held Value (ADR-0031): the engine block-slices at every clock change, so this
        // call sees one constant level. Compare it to the level held across the previous slice for
        // the edge; the slice's frame 0 *is* the change frame (block-absolute), so emitting there is
        // sample-accurate. The held latch carries `prev` across blocks/slices.
        let g = io.input::<f32>(IN_CLOCK).unwrap_or(0.0);
        let mut step = self.step;
        let mut held = self.held;
        match self.clock.detect(g) {
            Edge::Rising => {
                // End any held note, advance, and play the new step.
                if let Some(m) = held.take() {
                    io.output::<Note>(MSG_NOTES).emit(0, degree_note(m, 0.0));
                }
                step = (step + 1).rem_euclid(length);
                if let Some(m) = note_at(step) {
                    io.output::<Note>(MSG_NOTES).emit(0, degree_note(m, 1.0));
                    held = Some(m);
                }
            }
            Edge::Falling => {
                // Release the step's note (the per-beat pluck).
                if let Some(m) = held.take() {
                    io.output::<Note>(MSG_NOTES).emit(0, degree_note(m, 0.0));
                }
            }
            Edge::None => {}
        }
        self.step = step;
        self.held = held;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Sequencer);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Emit};
    use crate::op_driver::OpDriver;

    const SR: f32 = 48_000.0;

    /// Apply the `[length, step1..step16, gate_mode, pitch]` control layout to `d` as held
    /// controls: the Float entries via `set`, and `gate_mode` (index `NUM_STEPS + 1`) the held enum.
    fn set_controls(d: &mut OpDriver, controls: &[f32]) {
        d.set(IN_LENGTH, controls[0]);
        for k in 0..NUM_STEPS {
            d.set(in_step(k), controls[1 + k]);
        }
        let gate = controls[NUM_STEPS + 1] as usize == 1; // Degree=0, Gate=1
        d.set(
            IN_GATE_MODE,
            if gate {
                GateMode::Gate
            } else {
                GateMode::Degree
            },
        );
        d.set(IN_PITCH, controls[NUM_STEPS + 2]);
    }

    /// Drive a fresh Sequencer with `clock` (the Clock beat gate, a time-varying Buffer input) and
    /// the `[length, step1..step16, gate_mode, pitch]` control layout, through the real engine.
    /// Returns the emitted Messages (block-absolute frames).
    fn run(clock: &[f32], controls: &[f32]) -> Vec<Emit> {
        let mut d = OpDriver::for_type(Sequencer::new(), SR);
        set_controls(&mut d, controls);
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

    /// A clock gate: high for the first half of each `period`-sample beat, repeated.
    fn beat_gate(period: usize, beats: usize) -> Vec<f32> {
        let mut g = Vec::with_capacity(period * beats);
        for _ in 0..beats {
            for i in 0..period {
                g.push(if i < period / 2 { 1.0 } else { 0.0 });
            }
        }
        g
    }

    /// Build a degree-mode control vector: `length` + 16 step degrees (`gate_mode`=Degree(0),
    /// `pitch`=0).
    fn params(length: f32, pitches: [f32; NUM_STEPS]) -> Vec<f32> {
        let mut p = vec![length];
        p.extend_from_slice(&pitches);
        p.push(0.0); // gate_mode -> GateMode::Degree
        p.push(0.0); // pitch
        p
    }

    /// Build a gate-mode control vector: `length` + 16 boolean steps + `pitch` (`gate_mode`=Gate(1)).
    fn gate_params(length: f32, steps: [f32; NUM_STEPS], pitch: f32) -> Vec<f32> {
        let mut p = vec![length];
        p.extend_from_slice(&steps);
        p.push(1.0); // gate_mode -> GateMode::Gate
        p.push(pitch);
        p
    }

    /// The first 8 step degrees padded with 8 trailing zeros to the new 16-wide layout.
    fn pad8(first8: [f32; 8]) -> [f32; NUM_STEPS] {
        let mut s = [0.0f32; NUM_STEPS];
        s[..8].copy_from_slice(&first8);
        s
    }

    fn deg(e: &Emit) -> f32 {
        match &e.arg {
            Arg::Note(n) => n.pitch.degree().unwrap() as f32,
            other => panic!("expected a Note, got {other:?}"),
        }
    }
    fn vel(e: &Emit) -> f32 {
        match &e.arg {
            Arg::Note(n) => n.velocity,
            other => panic!("expected a Note, got {other:?}"),
        }
    }

    #[test]
    fn emits_note_on_at_the_downbeat_and_off_at_gate_fall() {
        // Clock starts high (downbeat) -> note-on for step 0 at frame 0; gate falls at period/2 ->
        // note-off there.
        let period = 100;
        let clock = beat_gate(period, 1);
        let degrees = pad8([0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let emits = run(&clock, &params(1.0, degrees));

        assert_eq!(emits.len(), 2, "one note-on + one note-off");
        assert_eq!(emits[0].frame, 0);
        approx::assert_relative_eq!(deg(&emits[0]), 0.0);
        approx::assert_relative_eq!(vel(&emits[0]), 1.0);
        assert_eq!(emits[1].frame, period / 2);
        approx::assert_relative_eq!(deg(&emits[1]), 0.0);
        approx::assert_relative_eq!(vel(&emits[1]), 0.0);
    }

    #[test]
    fn steps_advance_and_wrap() {
        // 3-step pattern over 4 beats: note-ons carry degrees 0, 1, 2, then wrap to 0.
        let period = 100;
        let clock = beat_gate(period, 4);
        let degrees = pad8([0.0, 1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let emits = run(&clock, &params(3.0, degrees));

        let ons: Vec<f32> = emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        assert_eq!(ons.len(), 4);
        approx::assert_relative_eq!(ons[0], 0.0);
        approx::assert_relative_eq!(ons[1], 1.0);
        approx::assert_relative_eq!(ons[2], 2.0);
        approx::assert_relative_eq!(ons[3], 0.0); // wrapped
    }

    #[test]
    fn rest_step_emits_no_note() {
        // Step 0 note, step 1 rest (-1): beat 1 emits nothing.
        let period = 100;
        let clock = beat_gate(period, 2);
        let degrees = pad8([0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let emits = run(&clock, &params(2.0, degrees));

        assert_eq!(emits.len(), 2);
        assert!(emits.iter().all(|e| e.frame < period));
    }

    #[test]
    fn step_state_is_continuous_across_calls() {
        // Splitting the clock across two blocks yields the same note-on degrees as one whole
        // block: the step machine and edge detection carry across the boundary.
        let period = 100;
        let degrees = pad8([0.0, 1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 0.0]);
        let p = params(4.0, degrees);
        let clock = beat_gate(period, 4);

        let ew = run(&clock, &p);
        let ons_whole: Vec<f32> = ew.iter().filter(|e| vel(e) > 0.5).map(deg).collect();

        // Two back-to-back renders on one driver: the step machine, edge detection, and held
        // clock level thread across the boundary exactly as within a render's 128-frame blocks.
        let mid = clock.len() / 2;
        let mut split = OpDriver::for_type(Sequencer::new(), SR);
        set_controls(&mut split, &p);
        let prev = push_clock(&mut split, &clock[..mid], 0.0);
        let e1 = split.render(mid).emits().to_vec();
        push_clock(&mut split, &clock[mid..], prev);
        let e2 = split.render(clock.len() - mid).emits().to_vec();
        let mut ons_split: Vec<f32> = e1.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        ons_split.extend(e2.iter().filter(|e| vel(e) > 0.5).map(deg));

        assert_eq!(ons_whole, ons_split);
    }

    #[test]
    fn spawned_sequencer_starts_before_the_first_step() {
        let degrees = pad8([0.0, 1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let p = params(3.0, degrees);
        let mut a = OpDriver::for_type(Sequencer::new(), SR);
        set_controls(&mut a, &p);
        let clock = beat_gate(100, 3);
        push_clock(&mut a, &clock, 0.0);
        a.render(clock.len());

        // The spawn is fresh: its first beat plays step 0 (degree 0) again, not where `a` ended.
        let mut b = a.spawn();
        set_controls(&mut b, &p);
        let one = beat_gate(100, 1);
        push_clock(&mut b, &one, 0.0);
        let emits = b.render(one.len()).emits().to_vec();
        let first_on = emits.iter().find(|e| vel(e) > 0.5).expect("a note-on");
        approx::assert_relative_eq!(deg(first_on), 0.0);
    }

    // --- V1.3 gate-mode + 16-step expansion (ADR-0022) ---

    #[test]
    fn default_descriptor_preserves_eight_step_behavior() {
        // The descriptor's input defaults (length 8, degree mode, ascending 0..7) must reproduce
        // the prior 8-step ascending pattern.
        let desc = Sequencer::descriptor();
        let length_default = desc.inputs[IN_LENGTH].meta.as_ref().unwrap().default;
        assert_eq!(length_default, 8.0, "default length stays 8");
        assert_eq!(
            desc.inputs[IN_GATE_MODE].enum_meta().unwrap().default,
            GateMode::Degree.to_index(),
            "default is degree mode"
        );

        let mut defaults = vec![length_default];
        for k in 0..NUM_STEPS {
            defaults.push(desc.inputs[in_step(k)].meta.as_ref().unwrap().default);
        }
        defaults.push(0.0); // gate_mode -> Degree
        defaults.push(desc.inputs[IN_PITCH].meta.as_ref().unwrap().default); // pitch

        let clock = beat_gate(100, 8);
        let emits = run(&clock, &defaults);
        let ons: Vec<f32> = emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        assert_eq!(ons, vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0]);
    }

    #[test]
    fn gate_mode_emits_pitch_on_hits_and_skips_off_steps() {
        // Boolean steps [1,0,1,0] at length 4, pitch degree 5: beats 0 and 2 hit (degree 5),
        // beats 1 and 3 are off — no note.
        let period = 100;
        let clock = beat_gate(period, 4);
        let steps = pad8([1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let emits = run(&clock, &gate_params(4.0, steps, 5.0));

        let ons: Vec<f32> = emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        assert_eq!(ons.len(), 2, "two hits (beats 0 and 2)");
        approx::assert_relative_eq!(ons[0], 5.0);
        approx::assert_relative_eq!(ons[1], 5.0);
    }

    #[test]
    fn gate_mode_off_step_releases_nothing() {
        // A single hit then an off step: on+off for beat 0, silence for beat 1.
        let period = 100;
        let clock = beat_gate(period, 2);
        let steps = pad8([1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let emits = run(&clock, &gate_params(2.0, steps, 0.0));
        assert_eq!(emits.len(), 2, "on+off for the single hit only");
        assert!(emits.iter().all(|e| e.frame < period));
    }

    #[test]
    fn sixteen_steps_all_play_at_full_length() {
        // length 16, gate mode, every step a hit: exactly 16 note-ons over 16 beats.
        let period = 64;
        let clock = beat_gate(period, 16);
        let steps = [1.0f32; NUM_STEPS];
        let emits = run(&clock, &gate_params(16.0, steps, 3.0));
        let ons = emits.iter().filter(|e| vel(e) > 0.5).count();
        assert_eq!(ons, 16, "all 16 steps hit");
        assert!(
            emits.iter().filter(|e| vel(e) > 0.5).all(|e| deg(e) == 3.0),
            "every hit plays the pitch degree"
        );
    }

    #[test]
    fn step9_through_16_carry_distinct_degrees_in_degree_mode() {
        // Degree mode, length 16, steps set to a rising run: the back half emits those degrees.
        let period = 64;
        let clock = beat_gate(period, 16);
        let mut steps = [0.0f32; NUM_STEPS];
        for (k, s) in steps.iter_mut().enumerate() {
            *s = k as f32; // step1→0, ..., step16→15
        }
        let emits = run(&clock, &params(16.0, steps));
        let ons: Vec<f32> = emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        assert_eq!(ons.len(), 16);
        for (k, &d) in ons.iter().enumerate() {
            approx::assert_relative_eq!(d, k as f32);
        }
    }
}
