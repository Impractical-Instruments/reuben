//! Sequencer — a clock-driven step sequencer that emits note Messages (V1.1, ADR-0014; V1.3
//! gate-mode + 16 steps, ADR-0022).
//!
//! Walks a fixed pattern, one step per beat, driven by the [`Clock`]'s beat `gate`: each rising
//! edge of the clock input advances to the next step (wrapping at `length`) and **emits a `note`
//! Message** for that step; the note is released when the beat gate falls. It is a note *source*
//! on the internal message graph — wire its `degrees` output to a
//! [`Voicer`](crate::operators::Voicer) (`sequencer.degrees → voicer.notes`) and the sequence is
//! polyphony-, transpose-, and snap-composable, exactly like notes arriving from outside.
//!
//! Two step interpretations, selected by `gate_mode` (ADR-0022):
//! - **degree mode** (`gate_mode` = 0, default): each `stepN` *is* the scale degree to play that
//!   beat; a value below 0 is a rest. The default pattern `[0..7]` under the default
//!   C-major/12-TET context is bit-identical to the prior MIDI default
//!   `[60,62,64,65,67,69,71,72]`.
//! - **gate mode** (`gate_mode` = 1): each `stepN` reads as a **boolean on/off** (≥ 0.5 = hit)
//!   and every hit emits the single per-lane `pitch` degree. This is the groove-box step grid —
//!   a row of toggles that all play one drum voice (ADR-0022).
//!
//! - input 0: `clock` (Signal) — the Clock's beat gate. A rising edge (crossing 0.5 upward)
//!   advances the step and emits a note-on; the following falling edge emits the note-off. The
//!   clock's previous level is held across blocks, so an edge straddling a block boundary fires
//!   exactly once.
//! - output 0 (Message): `degrees` — `degree` Messages, arg 0 = **scale degree**, arg 1 =
//!   velocity (1 = on, 0 = off). The Voicer resolves the degree through the tonal context.
//! - param 0: `length` — number of active steps (1..=16); the pattern wraps at it. Default 8.
//! - params 1..=16: `step1`..`step16` — per-step value (a degree in degree mode, a boolean hit
//!   in gate mode).
//! - param 17: `gate_mode` — 0 = degree (default), 1 = boolean gate.
//! - param 18: `pitch` — the degree emitted on each hit in gate mode (default 0 = root).
//!
//! Single-Lane by design (ADR-0014): emission happens pre-fan-out, a mono note line; the
//! downstream Voicer expands it to Voices. All params are ordinary, so a change is sample-accurate
//! via block-slicing; the step machine stays continuous across the cut.

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::message::Arg;
use crate::operator::{Io, Operator};

pub const IN_CLOCK: usize = 0;
/// Message output ordinal of the `degrees` port (the index [`Io::emit`] uses).
pub const MSG_NOTES: usize = 0;
pub const P_LENGTH: usize = 0;
/// Slot of the first step value; step `k` (0-based) is param `P_STEP0 + k`.
pub const P_STEP0: usize = 1;
/// Number of step slots in the pattern (V1.3: expanded 8 → 16, ADR-0022).
pub const NUM_STEPS: usize = 16;
/// Boolean-step mode toggle: 0 = degree (default), 1 = gate. Param index past the last step.
pub const P_GATE_MODE: usize = P_STEP0 + NUM_STEPS; // 17
/// The single degree emitted per hit in gate mode.
pub const P_PITCH: usize = P_GATE_MODE + 1; // 18

pub struct Sequencer {
    /// Index of the current step, or -1 before the first beat edge. Continuous across
    /// blocks. Advanced (and wrapped at `length`) on each rising edge of the clock input.
    step: i64,
    /// Clock input level at the previous sample, so a rising/falling edge is detected across
    /// the block boundary (and a clock that starts already-high fires its first edge at 0).
    prev_clock: f32,
    /// Scale degree currently sounding (emitted note-on, not yet note-off), for release.
    held: Option<f32>,
}

impl Default for Sequencer {
    fn default() -> Self {
        Self {
            step: -1,
            prev_clock: 0.0,
            held: None,
        }
    }
}

impl Sequencer {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Operator for Sequencer {
    fn descriptor() -> Descriptor {
        // Default pattern: an ascending one-octave scale by degree (0..7) on the first 8 steps,
        // so the instrument sings out of the box. Steps 9..16 default to degree 0; with the
        // default `length` of 8 they never play, so existing 8-step rigs stay bit-identical.
        const DEFAULT_DEGREES: [f32; NUM_STEPS] = [
            0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, // original 8
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, // new 9..16 (inert at default length 8)
        ];
        const STEP_NAMES: [&str; NUM_STEPS] = [
            "step1", "step2", "step3", "step4", "step5", "step6", "step7", "step8", "step9",
            "step10", "step11", "step12", "step13", "step14", "step15", "step16",
        ];
        let mut params = Vec::with_capacity(NUM_STEPS + 3);
        params.push(ParamMeta {
            name: "length",
            min: 1.0,
            max: NUM_STEPS as f32,
            // Default stays 8 (not 16) so existing instruments behave identically (ADR-0022).
            default: 8.0,
            unit: "steps",
            curve: Curve::Linear,
        });
        for (name, default) in STEP_NAMES.iter().zip(DEFAULT_DEGREES) {
            params.push(ParamMeta {
                name,
                min: -1.0,
                max: 24.0,
                default,
                unit: "degree",
                curve: Curve::Linear,
            });
        }
        params.push(ParamMeta {
            name: "gate_mode",
            min: 0.0,
            max: 1.0,
            default: 0.0,
            unit: "",
            curve: Curve::Linear,
        });
        params.push(ParamMeta {
            name: "pitch",
            min: -1.0,
            max: 24.0,
            default: 0.0,
            unit: "degree",
            curve: Curve::Linear,
        });
        Descriptor {
            type_name: "sequencer",
            inputs: vec![Port::signal("clock")],
            outputs: vec![Port::message("degrees")],
            params,
            resources: vec![],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let length = (io.param(P_LENGTH).round() as i64).clamp(1, NUM_STEPS as i64);
        let gate_mode = io.param(P_GATE_MODE) >= 0.5;
        let pitch = io.param(P_PITCH);

        // Snapshot the step values: constant for this (sub)block, and `io.param` can't be
        // read while emitting.
        let mut steps = [0.0f32; NUM_STEPS];
        for (k, p) in steps.iter_mut().enumerate() {
            *p = io.param(P_STEP0 + k);
        }
        // The degree to emit for a given step, or None for a rest / off-step.
        // - degree mode: the step value IS the degree; below 0 is a rest.
        // - gate mode: the step is a boolean hit (≥ 0.5); a hit emits the per-lane `pitch`,
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

        let mut step = self.step;
        let mut prev = self.prev_clock;
        let mut held = self.held;
        for i in 0..n {
            let g = io.input(IN_CLOCK).map_or(0.0, |c| c[i]);
            if prev < 0.5 && g >= 0.5 {
                // Rising edge: end any held note, advance, and play the new step.
                if let Some(m) = held.take() {
                    io.emit(MSG_NOTES, "degree", [Arg::Float(m), Arg::Float(0.0)], i);
                }
                step = (step + 1).rem_euclid(length);
                if let Some(m) = note_at(step) {
                    io.emit(MSG_NOTES, "degree", [Arg::Float(m), Arg::Float(1.0)], i);
                    held = Some(m);
                }
            } else if prev >= 0.5 && g < 0.5 {
                // Falling edge: release the step's note (the per-beat pluck).
                if let Some(m) = held.take() {
                    io.emit(MSG_NOTES, "degree", [Arg::Float(m), Arg::Float(0.0)], i);
                }
            }
            prev = g;
        }
        self.step = step;
        self.prev_clock = prev;
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
    use crate::message::Emit;
    use crate::operator::Io;

    const SR: f32 = 48_000.0;

    /// Run `seq` over one block of `clock` samples; returns the emitted Messages
    /// (block-absolute frames).
    fn run(seq: &mut Sequencer, clock: &[f32], params: &[f32]) -> Vec<Emit> {
        let n = clock.len();
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![]; // `degrees` is a Message port — no Signal buffer.
            let inputs: Vec<Option<&[f32]>> = vec![Some(clock)];
            let mut io = Io::new(SR, n, inputs, outs, params, &[]).with_emit(&mut emits, 0);
            seq.process(&mut io);
        }
        emits
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

    /// Build a degree-mode param vector: `length` + 16 step degrees (`gate_mode`=0, `pitch`=0).
    fn params(length: f32, pitches: [f32; NUM_STEPS]) -> Vec<f32> {
        let mut p = vec![length];
        p.extend_from_slice(&pitches);
        p.push(0.0); // gate_mode
        p.push(0.0); // pitch
        p
    }

    /// Build a gate-mode param vector: `length` + 16 boolean steps + `pitch`.
    fn gate_params(length: f32, steps: [f32; NUM_STEPS], pitch: f32) -> Vec<f32> {
        let mut p = vec![length];
        p.extend_from_slice(&steps);
        p.push(1.0); // gate_mode
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
        e.args[0].as_f32().unwrap()
    }
    fn vel(e: &Emit) -> f32 {
        e.args[1].as_f32().unwrap()
    }

    #[test]
    fn emits_note_on_at_the_downbeat_and_off_at_gate_fall() {
        // Clock starts high (downbeat) -> note-on for step 0 at frame 0; gate falls at
        // period/2 -> note-off there.
        let period = 100;
        let clock = beat_gate(period, 1);
        let degrees = pad8([0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &params(1.0, degrees));

        assert_eq!(emits.len(), 2, "one note-on + one note-off");
        assert_eq!(emits[0].addr, "degree");
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
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &params(3.0, degrees));

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
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &params(2.0, degrees));

        // Only beat 0's on+off; beat 1 (rest) is silent.
        assert_eq!(emits.len(), 2);
        assert!(emits.iter().all(|e| e.frame < period));
    }

    #[test]
    fn step_state_is_continuous_across_calls() {
        // Splitting the clock across two blocks yields the same note-on degrees as one
        // whole block: the step machine and edge detection carry across the boundary.
        let period = 100;
        let degrees = pad8([0.0, 1.0, 2.0, 3.0, 0.0, 0.0, 0.0, 0.0]);
        let p = params(4.0, degrees);
        let clock = beat_gate(period, 4);

        let mut whole = Sequencer::new();
        let ew = run(&mut whole, &clock, &p);
        let ons_whole: Vec<f32> = ew.iter().filter(|e| vel(e) > 0.5).map(deg).collect();

        let mid = clock.len() / 2;
        let mut split = Sequencer::new();
        let e1 = run(&mut split, &clock[..mid], &p);
        let e2 = run(&mut split, &clock[mid..], &p);
        let mut ons_split: Vec<f32> = e1.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        ons_split.extend(e2.iter().filter(|e| vel(e) > 0.5).map(deg));

        assert_eq!(ons_whole, ons_split);
    }

    #[test]
    fn spawned_sequencer_starts_before_the_first_step() {
        let mut a = Sequencer::new();
        let clock = beat_gate(100, 3);
        let degrees = pad8([0.0, 1.0, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let _ = run(&mut a, &clock, &params(3.0, degrees));
        let mut b = a.spawn();
        // The spawn is fresh: its first beat plays step 0 (degree 0) again, not where `a` ended.
        let one = beat_gate(100, 1);
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            let inputs: Vec<Option<&[f32]>> = vec![Some(&one[..])];
            let p = params(3.0, degrees);
            let mut io = Io::new(SR, one.len(), inputs, outs, &p, &[]).with_emit(&mut emits, 0);
            b.process(&mut io);
        }
        let first_on = emits.iter().find(|e| vel(e) > 0.5).expect("a note-on");
        approx::assert_relative_eq!(deg(first_on), 0.0);
    }

    // --- V1.3 gate-mode + 16-step expansion (ADR-0022) ---

    #[test]
    fn default_descriptor_preserves_eight_step_behavior() {
        // The descriptor's defaults (length 8, degree mode, ascending 0..7) must reproduce the
        // prior 8-step ascending pattern bit-for-bit, proving existing rigs are unchanged.
        let defaults: Vec<f32> = Sequencer::descriptor()
            .params
            .iter()
            .map(|p| p.default)
            .collect();
        assert_eq!(
            defaults.len(),
            NUM_STEPS + 3,
            "length + 16 steps + mode + pitch"
        );
        assert_eq!(defaults[P_LENGTH], 8.0, "default length stays 8");
        assert_eq!(defaults[P_GATE_MODE], 0.0, "default is degree mode");

        let clock = beat_gate(100, 8);
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &defaults);
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
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &gate_params(4.0, steps, 5.0));

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
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &gate_params(2.0, steps, 0.0));
        assert_eq!(emits.len(), 2, "on+off for the single hit only");
        assert!(emits.iter().all(|e| e.frame < period));
    }

    #[test]
    fn sixteen_steps_all_play_at_full_length() {
        // length 16, gate mode, every step a hit: exactly 16 note-ons over 16 beats — proves
        // the new step9..16 slots are reachable.
        let period = 64;
        let clock = beat_gate(period, 16);
        let steps = [1.0f32; NUM_STEPS];
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &gate_params(16.0, steps, 3.0));
        let ons = emits.iter().filter(|e| vel(e) > 0.5).count();
        assert_eq!(ons, 16, "all 16 steps hit");
        assert!(
            emits.iter().filter(|e| vel(e) > 0.5).all(|e| deg(e) == 3.0),
            "every hit plays the pitch degree"
        );
    }

    #[test]
    fn step9_through_16_carry_distinct_degrees_in_degree_mode() {
        // Degree mode, length 16, steps 9..16 set to a descending run: the back half emits
        // those degrees, proving the expanded slots index correctly.
        let period = 64;
        let clock = beat_gate(period, 16);
        let mut steps = [0.0f32; NUM_STEPS];
        for (k, s) in steps.iter_mut().enumerate() {
            *s = k as f32; // step1→0, ..., step16→15
        }
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &params(16.0, steps));
        let ons: Vec<f32> = emits.iter().filter(|e| vel(e) > 0.5).map(deg).collect();
        assert_eq!(ons.len(), 16);
        for (k, &d) in ons.iter().enumerate() {
            approx::assert_relative_eq!(d, k as f32);
        }
    }
}
