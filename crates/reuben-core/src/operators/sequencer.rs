//! Sequencer — a clock-driven step sequencer that emits note Messages (V1.1, ADR-0014).
//!
//! Walks a fixed pattern of pitches, one step per beat, driven by the [`Clock`]'s beat
//! `gate`: each rising edge of the clock input advances to the next step (wrapping at
//! `length`) and **emits a `note` Message** for that step; the note is released when the
//! beat gate falls. It is a note *source* on the internal message graph — wire its `notes`
//! output to a [`Voicer`](crate::operators::Voicer) (`sequencer.notes → voicer.notes`) and
//! the sequence is polyphony-, transpose-, and snap-composable, exactly like notes arriving
//! from outside.
//!
//! - input 0: `clock` (Signal) — the Clock's beat gate. A rising edge (crossing 0.5 upward)
//!   advances the step and emits a note-on; the following falling edge emits the note-off.
//!   The clock's previous level is held across blocks, so an edge straddling a block
//!   boundary fires exactly once.
//! - output 0 (Message): `notes` — `note` Messages, arg 0 = MIDI note, arg 1 = velocity
//!   (1 = on, 0 = off); the Voicer's note format.
//! - param 0: `length` — number of active steps (1..=8); the pattern wraps at it.
//! - params 1..=8: `step1`..`step8` — MIDI note for each step. A value below 0 is a rest
//!   (no note emitted that beat).
//!
//! Single-Lane by design (ADR-0014): emission happens pre-fan-out, a mono note line; the
//! downstream Voicer expands it to Voices. `length`/pitches are ordinary params, so a change
//! is sample-accurate via block-slicing; the step machine stays continuous across the cut.

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::message::Arg;
use crate::operator::{Io, Operator};

pub const IN_CLOCK: usize = 0;
/// Message output ordinal of the `notes` port (the index [`Io::emit`] uses).
pub const MSG_NOTES: usize = 0;
pub const P_LENGTH: usize = 0;
/// Slot of the first step pitch; step `k` (0-based) is param `P_STEP0 + k`.
pub const P_STEP0: usize = 1;
/// Number of step slots in the pattern.
pub const NUM_STEPS: usize = 8;

pub struct Sequencer {
    /// Index of the current step, or -1 before the first beat edge. Continuous across
    /// blocks. Advanced (and wrapped at `length`) on each rising edge of the clock input.
    step: i64,
    /// Clock input level at the previous sample, so a rising/falling edge is detected across
    /// the block boundary (and a clock that starts already-high fires its first edge at 0).
    prev_clock: f32,
    /// MIDI note currently sounding (emitted note-on, not yet note-off), for release.
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
        // Default pattern: a one-octave C-major scale, so the instrument sings out of the
        // box rather than sitting silent.
        const DEFAULT_PITCHES: [f32; NUM_STEPS] = [60.0, 62.0, 64.0, 65.0, 67.0, 69.0, 71.0, 72.0];
        let mut params = Vec::with_capacity(NUM_STEPS + 1);
        params.push(ParamMeta {
            name: "length",
            min: 1.0,
            max: NUM_STEPS as f32,
            default: NUM_STEPS as f32,
            unit: "steps",
            curve: Curve::Linear,
        });
        const STEP_NAMES: [&str; NUM_STEPS] = [
            "step1", "step2", "step3", "step4", "step5", "step6", "step7", "step8",
        ];
        for (name, default) in STEP_NAMES.iter().zip(DEFAULT_PITCHES) {
            params.push(ParamMeta {
                name,
                min: -1.0,
                max: 127.0,
                default,
                unit: "MIDI",
                curve: Curve::Linear,
            });
        }
        Descriptor {
            type_name: "sequencer",
            inputs: vec![Port::signal("clock")],
            outputs: vec![Port::message("notes")],
            params,
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let length = (io.param(P_LENGTH).round() as i64).clamp(1, NUM_STEPS as i64);

        // Snapshot the step pitches: constant for this (sub)block, and `io.param` can't be
        // read while emitting.
        let mut pitches = [0.0f32; NUM_STEPS];
        for (k, p) in pitches.iter_mut().enumerate() {
            *p = io.param(P_STEP0 + k);
        }
        let note_at = |step: i64| -> Option<f32> {
            if step < 0 {
                return None;
            }
            let midi = pitches[(step as usize) % NUM_STEPS];
            (midi >= 0.0).then_some(midi)
        };

        let mut step = self.step;
        let mut prev = self.prev_clock;
        let mut held = self.held;
        for i in 0..n {
            let g = io.input(IN_CLOCK).map_or(0.0, |c| c[i]);
            if prev < 0.5 && g >= 0.5 {
                // Rising edge: end any held note, advance, and play the new step.
                if let Some(m) = held.take() {
                    io.emit(MSG_NOTES, "note", [Arg::Float(m), Arg::Float(0.0)], i);
                }
                step = (step + 1).rem_euclid(length);
                if let Some(m) = note_at(step) {
                    io.emit(MSG_NOTES, "note", [Arg::Float(m), Arg::Float(1.0)], i);
                    held = Some(m);
                }
            } else if prev >= 0.5 && g < 0.5 {
                // Falling edge: release the step's note (the per-beat pluck).
                if let Some(m) = held.take() {
                    io.emit(MSG_NOTES, "note", [Arg::Float(m), Arg::Float(0.0)], i);
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
            let outs: Vec<&mut [f32]> = vec![]; // `notes` is a Message port — no Signal buffer.
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

    fn params(length: f32, pitches: [f32; NUM_STEPS]) -> Vec<f32> {
        let mut p = vec![length];
        p.extend_from_slice(&pitches);
        p
    }

    fn midi(e: &Emit) -> f32 {
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
        let pitches = [60.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &params(1.0, pitches));

        assert_eq!(emits.len(), 2, "one note-on + one note-off");
        assert_eq!(emits[0].addr, "note");
        assert_eq!(emits[0].frame, 0);
        approx::assert_relative_eq!(midi(&emits[0]), 60.0);
        approx::assert_relative_eq!(vel(&emits[0]), 1.0);
        assert_eq!(emits[1].frame, period / 2);
        approx::assert_relative_eq!(midi(&emits[1]), 60.0);
        approx::assert_relative_eq!(vel(&emits[1]), 0.0);
    }

    #[test]
    fn steps_advance_and_wrap() {
        // 3-step pattern over 4 beats: note-ons carry 60, 62, 64, then wrap to 60.
        let period = 100;
        let clock = beat_gate(period, 4);
        let pitches = [60.0, 62.0, 64.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &params(3.0, pitches));

        let ons: Vec<f32> = emits.iter().filter(|e| vel(e) > 0.5).map(midi).collect();
        assert_eq!(ons.len(), 4);
        approx::assert_relative_eq!(ons[0], 60.0);
        approx::assert_relative_eq!(ons[1], 62.0);
        approx::assert_relative_eq!(ons[2], 64.0);
        approx::assert_relative_eq!(ons[3], 60.0); // wrapped
    }

    #[test]
    fn rest_step_emits_no_note() {
        // Step 0 note, step 1 rest (-1): beat 1 emits nothing.
        let period = 100;
        let clock = beat_gate(period, 2);
        let pitches = [60.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut seq = Sequencer::new();
        let emits = run(&mut seq, &clock, &params(2.0, pitches));

        // Only beat 0's on+off; beat 1 (rest) is silent.
        assert_eq!(emits.len(), 2);
        assert!(emits.iter().all(|e| e.frame < period));
    }

    #[test]
    fn step_state_is_continuous_across_calls() {
        // Splitting the clock across two blocks yields the same note-on pitches as one
        // whole block: the step machine and edge detection carry across the boundary.
        let period = 100;
        let pitches = [60.0, 62.0, 64.0, 65.0, 0.0, 0.0, 0.0, 0.0];
        let p = params(4.0, pitches);
        let clock = beat_gate(period, 4);

        let mut whole = Sequencer::new();
        let ew = run(&mut whole, &clock, &p);
        let ons_whole: Vec<f32> = ew.iter().filter(|e| vel(e) > 0.5).map(midi).collect();

        let mid = clock.len() / 2;
        let mut split = Sequencer::new();
        let e1 = run(&mut split, &clock[..mid], &p);
        let e2 = run(&mut split, &clock[mid..], &p);
        let mut ons_split: Vec<f32> = e1.iter().filter(|e| vel(e) > 0.5).map(midi).collect();
        ons_split.extend(e2.iter().filter(|e| vel(e) > 0.5).map(midi));

        assert_eq!(ons_whole, ons_split);
    }

    #[test]
    fn spawned_sequencer_starts_before_the_first_step() {
        let mut a = Sequencer::new();
        let clock = beat_gate(100, 3);
        let pitches = [60.0, 62.0, 64.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let _ = run(&mut a, &clock, &params(3.0, pitches));
        let mut b = a.spawn();
        // The spawn is fresh: its first beat plays step 0 (60) again, not where `a` ended.
        let one = beat_gate(100, 1);
        let mut emits: Vec<Emit> = Vec::new();
        {
            let outs: Vec<&mut [f32]> = vec![];
            let inputs: Vec<Option<&[f32]>> = vec![Some(&one[..])];
            let p = params(3.0, pitches);
            let mut io = Io::new(SR, one.len(), inputs, outs, &p, &[]).with_emit(&mut emits, 0);
            b.process(&mut io);
        }
        let first_on = emits.iter().find(|e| vel(e) > 0.5).expect("a note-on");
        approx::assert_relative_eq!(midi(first_on), 60.0);
    }
}
