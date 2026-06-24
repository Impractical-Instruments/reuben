//! Sequencer — a clock-driven step sequencer that emits note Messages (V1.1, ADR-0014; V1.3
//! gate-mode + 16 steps, ADR-0022; shape model, ADR-0028).
//!
//! Walks a fixed pattern, one step per beat, driven by the [`Clock`]'s beat `gate`: each rising
//! edge of the clock input advances to the next step (wrapping at `length`) and **emits a `note`
//! Message** for that step; the note is released when the beat gate falls. It is a note *source*
//! on the internal message graph — wire its `degrees` output to a
//! [`Voicer`](crate::operators::Voicer) (`sequencer.degrees → voicer.notes`) and the sequence is
//! polyphony-, transpose-, and snap-composable, exactly like notes arriving from outside.
//!
//! Shape model (ADR-0028): `length`, `step1`..`step16`, and `pitch` are **`Float` inputs**, each
//! owning its unwired default — read block-rate via `io.value`. `gate_mode` is an **`Enum` input**
//! {`Degree`, `Gate`}: a held, live-switchable choice read via `io.enum_index`. `clock` is a bare
//! `Float` wire-in, read per-sample via `io.signal` for edge detection.
//!
//! Two step interpretations, selected by `gate_mode` (ADR-0022):
//! - **degree mode** (`Degree`, default): each `stepN` *is* the scale degree to play that beat; a
//!   value below 0 is a rest. The default pattern `[0..7]` under the default C-major/12-TET
//!   context is bit-identical to the prior MIDI default `[60,62,64,65,67,69,71,72]`.
//! - **gate mode** (`Gate`): each `stepN` reads as a **boolean on/off** (≥ 0.5 = hit) and every
//!   hit emits the single per-lane `pitch` degree. This is the groove-box step grid — a row of
//!   toggles that all play one drum voice (ADR-0022).
//!
//! - input 0: `clock` (`Float`) — the Clock's beat gate. A rising edge (crossing 0.5 upward)
//!   advances the step and emits a note-on; the following falling edge emits the note-off. The
//!   clock's previous level is held across blocks, so an edge straddling a block boundary fires
//!   exactly once.
//! - inputs 1..=17: `length` (number of active steps 1..=16; default 8) then `step1`..`step16`
//!   (per-step value — a degree in degree mode, a boolean hit in gate mode).
//! - input 18: `gate_mode` (`Enum` {Degree, Gate}) — `Degree` is the default.
//! - input 19: `pitch` (`Float`) — the degree emitted on each hit in gate mode (default 0 = root).
//! - output 0 (Message): `degrees` — `degree` Messages, arg 0 = **scale degree**, arg 1 =
//!   velocity (1 = on, 0 = off). The Voicer resolves the degree through the tonal context.
//!
//! Single-Lane by design (ADR-0014): emission happens pre-fan-out, a mono note line; the
//! downstream Voicer expands it to Voices. The Float inputs are read block-rate, so a change is
//! sample-accurate via block-slicing; the step machine stays continuous across the cut.

use crate::descriptor::Descriptor;
use crate::message::Arg;
use crate::operator::{Io, Operator};

/// Number of step slots in the pattern (V1.3: expanded 8 → 16, ADR-0022).
pub const NUM_STEPS: usize = 16;

// Single-source contract (ADR-0025/0028): one declaration -> IN_/OUT_ consts, the `GateMode` enum
// type, and the Descriptor; no drift.
crate::operator_contract!(Sequencer {
    inputs:  { clock:  float,
               length: float { 1.0..=16.0, default 8.0, "steps", lin },
               step1:  float { -1.0..=24.0, default 0.0, "degree", lin },
               step2:  float { -1.0..=24.0, default 1.0, "degree", lin },
               step3:  float { -1.0..=24.0, default 2.0, "degree", lin },
               step4:  float { -1.0..=24.0, default 3.0, "degree", lin },
               step5:  float { -1.0..=24.0, default 4.0, "degree", lin },
               step6:  float { -1.0..=24.0, default 5.0, "degree", lin },
               step7:  float { -1.0..=24.0, default 6.0, "degree", lin },
               step8:  float { -1.0..=24.0, default 7.0, "degree", lin },
               step9:  float { -1.0..=24.0, default 0.0, "degree", lin },
               step10: float { -1.0..=24.0, default 0.0, "degree", lin },
               step11: float { -1.0..=24.0, default 0.0, "degree", lin },
               step12: float { -1.0..=24.0, default 0.0, "degree", lin },
               step13: float { -1.0..=24.0, default 0.0, "degree", lin },
               step14: float { -1.0..=24.0, default 0.0, "degree", lin },
               step15: float { -1.0..=24.0, default 0.0, "degree", lin },
               step16: float { -1.0..=24.0, default 0.0, "degree", lin },
               gate_mode: enum { Degree, Gate },
               pitch:  float { -1.0..=24.0, default 0.0, "degree", lin } },
    outputs: { degrees: message },
});

/// Message output ordinal of the `degrees` port (the index [`Io::emit`] uses).
pub const MSG_NOTES: usize = OUT_DEGREES;

/// Per-sample value of step `k` (0-based): input `IN_STEP1 + k`.
const fn in_step(k: usize) -> usize {
    IN_STEP1 + k
}

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
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let length = (io.value(IN_LENGTH).round() as i64).clamp(1, NUM_STEPS as i64);
        let gate_mode =
            GateMode::from_index(io.enum_index(IN_GATE_MODE)).unwrap_or_default() == GateMode::Gate;
        let pitch = io.value(IN_PITCH);

        // Snapshot the step values: constant for this (sub)block, and a Float input can't be
        // read while emitting.
        let mut steps = [0.0f32; NUM_STEPS];
        for (k, p) in steps.iter_mut().enumerate() {
            *p = io.value(in_step(k));
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
            let g = io.signal(IN_CLOCK).get(i).copied().unwrap_or(0.0);
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
    ///
    /// `controls` carries the former param layout `[length, step1..step16, gate_mode, pitch]`. The
    /// Float inputs (length, steps, pitch) are materialized as per-sample buffers `vec![v; n]` in
    /// port order; `gate_mode` is supplied as a held `Enum` index via `.with_enums` (ADR-0028).
    fn run(seq: &mut Sequencer, clock: &[f32], controls: &[f32]) -> Vec<Emit> {
        let n = clock.len();
        let mut emits: Vec<Emit> = Vec::new();
        // length + 16 steps come first; gate_mode and pitch are the last two entries.
        let pitch = controls[NUM_STEPS + 2];
        let mode_index = controls[NUM_STEPS + 1] as usize; // Degree=0, Gate=1
                                                           // Float buffers in port order: clock, length, step1..step16, (gate_mode), pitch.
        let float_bufs: Vec<Vec<f32>> = (0..=NUM_STEPS)
            .map(|k| vec![controls[k]; n]) // length, then the 16 steps
            .chain(std::iter::once(vec![pitch; n]))
            .collect();
        {
            let outs: Vec<&mut [f32]> = vec![]; // `degrees` is a Message port — no Signal buffer.
                                                // Ports: clock(0), length(1), step1..step16(2..=17), gate_mode(18), pitch(19).
            let mut inputs: Vec<Option<&[f32]>> = vec![Some(clock)];
            for b in &float_bufs[..=NUM_STEPS] {
                inputs.push(Some(b.as_slice())); // length + 16 steps
            }
            inputs.push(None); // gate_mode is an Enum input — no Float buffer
            inputs.push(Some(float_bufs[NUM_STEPS + 1].as_slice())); // pitch
                                                                     // Held enum index at the IN_GATE_MODE slot; other slots 0.
            let mut enums = [0usize; 20];
            enums[IN_GATE_MODE] = mode_index;
            let mut io = Io::new(SR, n, inputs, outs, &[], &[])
                .with_emit(&mut emits, 0)
                .with_enums(&enums);
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

    /// Build a degree-mode control vector: `length` + 16 step degrees (`gate_mode`=Degree(0),
    /// `pitch`=0). `run` materializes the Float entries as buffers and the `gate_mode` entry as a
    /// held `Enum` index.
    fn params(length: f32, pitches: [f32; NUM_STEPS]) -> Vec<f32> {
        let mut p = vec![length];
        p.extend_from_slice(&pitches);
        p.push(0.0); // gate_mode -> GateMode::Degree
        p.push(0.0); // pitch
        p
    }

    /// Build a gate-mode control vector: `length` + 16 boolean steps + `pitch`
    /// (`gate_mode`=Gate(1)).
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
            let n = one.len();
            let p = params(3.0, degrees);
            // length + 16 steps as Float buffers, then pitch; gate_mode via held Enum index.
            let float_bufs: Vec<Vec<f32>> = (0..=NUM_STEPS)
                .map(|k| vec![p[k]; n])
                .chain(std::iter::once(vec![p[NUM_STEPS + 2]; n]))
                .collect();
            let mut inputs: Vec<Option<&[f32]>> = vec![Some(&one[..])];
            for buf in &float_bufs[..=NUM_STEPS] {
                inputs.push(Some(buf.as_slice()));
            }
            inputs.push(None); // gate_mode (Enum)
            inputs.push(Some(float_bufs[NUM_STEPS + 1].as_slice())); // pitch
            let enums = [0usize; 20];
            let mut io = Io::new(SR, n, inputs, outs, &[], &[])
                .with_emit(&mut emits, 0)
                .with_enums(&enums);
            b.process(&mut io);
        }
        let first_on = emits.iter().find(|e| vel(e) > 0.5).expect("a note-on");
        approx::assert_relative_eq!(deg(first_on), 0.0);
    }

    // --- V1.3 gate-mode + 16-step expansion (ADR-0022) ---

    #[test]
    fn default_descriptor_preserves_eight_step_behavior() {
        // The descriptor's input defaults (length 8, degree mode, ascending 0..7) must reproduce
        // the prior 8-step ascending pattern bit-for-bit, proving existing rigs are unchanged.
        // Defaults now live on the Float/Enum inputs (ADR-0028), not a `params` block.
        let desc = Sequencer::descriptor();
        // Float-input defaults in port order: length(1), step1..step16(2..=17), pitch(19).
        let length_default = desc.inputs[IN_LENGTH].meta.as_ref().unwrap().default;
        assert_eq!(length_default, 8.0, "default length stays 8");
        assert_eq!(
            desc.inputs[IN_GATE_MODE]
                .enum_meta
                .as_ref()
                .unwrap()
                .default,
            GateMode::Degree.to_index(),
            "default is degree mode"
        );

        // Rebuild the former `[length, step1..step16, gate_mode, pitch]` control vector from the
        // input defaults (gate_mode 0 = Degree).
        let mut defaults = vec![length_default];
        for k in 0..NUM_STEPS {
            defaults.push(desc.inputs[in_step(k)].meta.as_ref().unwrap().default);
        }
        defaults.push(0.0); // gate_mode -> Degree
        defaults.push(desc.inputs[IN_PITCH].meta.as_ref().unwrap().default); // pitch

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
