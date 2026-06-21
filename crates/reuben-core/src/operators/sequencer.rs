//! Sequencer — a clock-driven step sequencer (V1.1).
//!
//! Walks a fixed pattern of pitches, one step per beat, driven by the [`Clock`]'s beat
//! `gate`: each rising edge of the clock input advances to the next step (wrapping at
//! `length`). It is a Signal-domain source — it emits the same `freq` + `gate` Signal pair
//! as the [`Voicer`](crate::operators::Voicer), so it drops straight into an oscillator +
//! envelope chain in its place, turning the Clock's bare beat grid into a melody.
//!
//! Operators cannot emit Messages (Render only routes external block-input Messages to
//! nodes), so the Sequencer drives downstream voices through Signal edges rather than by
//! synthesising note Messages into a Voicer — which also keeps it allocation-free and
//! deterministic. It is mono (one Lane): a melodic line, not a polyphonic source.
//!
//! - input 0: `clock` (Signal) — the Clock's beat gate. A rising edge (crossing 0.5
//!   upward) advances the step. Hold its previous level across blocks so an edge that
//!   straddles a block boundary still fires exactly once.
//! - output 0: `freq` (Signal) — the current step's frequency in Hz, held until the next
//!   step. A rest (or the pre-first-step state) emits 0.
//! - output 1: `gate` (Signal) — the clock gate passed through while the current step is a
//!   note; held low for a rest. So an active step plays for the clock gate's high portion
//!   (the first half of the beat) — a per-step pluck — and a rest is silent.
//! - param 0: `length` — number of active steps (1..=8); the pattern wraps at it.
//! - params 1..=8: `step1`..`step8` — MIDI note for each step. A value below 0 is a rest.
//!
//! `length` and the step pitches are ordinary params, so the engine block-slices on a
//! change and it takes effect at the exact sample. Step state stays continuous across the
//! cut, exactly like the Clock's phase.

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::operator::{Io, Operator};
use crate::pitch::Pitch;
use crate::tuning::{Tuning, TwelveTet};

pub const IN_CLOCK: usize = 0;
pub const OUT_FREQ: usize = 0;
pub const OUT_GATE: usize = 1;
pub const P_LENGTH: usize = 0;
/// Slot of the first step pitch; step `k` (0-based) is param `P_STEP0 + k`.
pub const P_STEP0: usize = 1;
/// Number of step slots in the pattern.
pub const NUM_STEPS: usize = 8;

pub struct Sequencer {
    tuning: TwelveTet,
    /// Index of the current step, or -1 before the first beat edge. Continuous across
    /// blocks. Advanced (and wrapped at `length`) on each rising edge of the clock input.
    step: i64,
    /// Clock input level at the previous sample, so a rising edge is detected across the
    /// block boundary (and a clock that starts already-high fires its first edge at 0).
    prev_clock: f32,
}

impl Default for Sequencer {
    fn default() -> Self {
        Self {
            tuning: TwelveTet::default(),
            step: -1,
            prev_clock: 0.0,
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
            outputs: vec![Port::signal("freq"), Port::signal("gate")],
            params,
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let length = (io.param(P_LENGTH).round() as i64).clamp(1, NUM_STEPS as i64);

        // Snapshot the step pitches up front — `io.param` can't be read while an output
        // borrow is live, and the value is constant for this (sub)block anyway.
        let mut pitches = [0.0f32; NUM_STEPS];
        for (k, p) in pitches.iter_mut().enumerate() {
            *p = io.param(P_STEP0 + k);
        }

        // MIDI of `step`, or `None` for a rest / the pre-first-step state.
        let note_at = |step: i64| -> Option<f32> {
            if step < 0 {
                return None;
            }
            let midi = pitches[(step as usize) % NUM_STEPS];
            (midi >= 0.0).then_some(midi)
        };

        let start_step = self.step;
        let start_prev = self.prev_clock;

        // Two passes (one per output port — input and output both borrow `io`, so each
        // sample reads the clock as a fresh short borrow before taking the output, the
        // delay-operator pattern). Both replay the identical step machine from the same
        // start state, so they stay in lock-step; the second commits the end state.
        {
            let mut step = start_step;
            let mut prev = start_prev;
            for i in 0..n {
                let g = io.input(IN_CLOCK).map_or(0.0, |c| c[i]);
                if prev < 0.5 && g >= 0.5 {
                    step = (step + 1).rem_euclid(length);
                }
                prev = g;
                io.output(OUT_FREQ)[i] =
                    note_at(step).map_or(0.0, |midi| self.tuning.hz(Pitch::from_midi(midi)));
            }
        }
        let (end_step, end_prev);
        {
            let mut step = start_step;
            let mut prev = start_prev;
            for i in 0..n {
                let g = io.input(IN_CLOCK).map_or(0.0, |c| c[i]);
                if prev < 0.5 && g >= 0.5 {
                    step = (step + 1).rem_euclid(length);
                }
                prev = g;
                // Pass the clock gate through only while the step is a note.
                io.output(OUT_GATE)[i] = if g >= 0.5 && note_at(step).is_some() {
                    1.0
                } else {
                    0.0
                };
            }
            end_step = step;
            end_prev = prev;
        }
        self.step = end_step;
        self.prev_clock = end_prev;
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::Io;

    const SR: f32 = 48_000.0;

    fn hz(midi: f32) -> f32 {
        TwelveTet::default().hz(Pitch::from_midi(midi))
    }

    /// Run `seq` over one block of `clock` samples with the given params; returns
    /// (freq, gate).
    fn run(seq: &mut Sequencer, clock: &[f32], params: &[f32]) -> (Vec<f32>, Vec<f32>) {
        let n = clock.len();
        let mut freq = vec![0.0f32; n];
        let mut gate = vec![0.0f32; n];
        {
            let outs: Vec<&mut [f32]> = vec![&mut freq[..], &mut gate[..]];
            let inputs: Vec<Option<&[f32]>> = vec![Some(clock)];
            let mut io = Io::new(SR, n, inputs, outs, params, &[]);
            seq.process(&mut io);
        }
        (freq, gate)
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

    /// All-steps-present default-ish params with the given length and pitches.
    fn params(length: f32, pitches: [f32; NUM_STEPS]) -> Vec<f32> {
        let mut p = vec![length];
        p.extend_from_slice(&pitches);
        p
    }

    #[test]
    fn steps_advance_on_each_clock_rising_edge() {
        // 4 beats of a 3-step pattern: steps land on 0,1,2 then wrap to 0.
        let period = 100;
        let clock = beat_gate(period, 4);
        let pitches = [60.0, 62.0, 64.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut seq = Sequencer::new();
        let (freq, _gate) = run(&mut seq, &clock, &params(3.0, pitches));

        // Sample mid-first-half of each beat (gate high, freq settled).
        let at = |beat: usize| freq[beat * period + 10];
        approx::assert_relative_eq!(at(0), hz(60.0), epsilon = 1e-2);
        approx::assert_relative_eq!(at(1), hz(62.0), epsilon = 1e-2);
        approx::assert_relative_eq!(at(2), hz(64.0), epsilon = 1e-2);
        approx::assert_relative_eq!(at(3), hz(60.0), epsilon = 1e-2); // wrapped
    }

    #[test]
    fn gate_passes_clock_through_for_notes_and_stays_low_for_rests() {
        // Step 0 is a note, step 1 is a rest (-1).
        let period = 100;
        let clock = beat_gate(period, 2);
        let pitches = [60.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut seq = Sequencer::new();
        let (_freq, gate) = run(&mut seq, &clock, &params(2.0, pitches));

        // Beat 0 (note): gate mirrors the clock — high first half, low second half.
        assert_eq!(gate[10], 1.0);
        assert_eq!(gate[period / 2 + 10], 0.0);
        // Beat 1 (rest): gate low throughout, even while the clock gate is high.
        assert_eq!(gate[period + 10], 0.0);
    }

    #[test]
    fn rest_step_emits_zero_freq() {
        let period = 100;
        let clock = beat_gate(period, 1);
        let pitches = [-1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut seq = Sequencer::new();
        let (freq, _gate) = run(&mut seq, &clock, &params(1.0, pitches));
        assert_eq!(freq[10], 0.0);
    }

    #[test]
    fn step_state_is_continuous_across_calls() {
        // An edge that straddles a block boundary advances exactly once: splitting the
        // clock into two halves yields the same steps as one whole block.
        let period = 100;
        let pitches = [60.0, 62.0, 64.0, 65.0, 0.0, 0.0, 0.0, 0.0];
        let p = params(4.0, pitches);
        let clock = beat_gate(period, 4);

        let mut whole = Sequencer::new();
        let (fw, _) = run(&mut whole, &clock, &p);

        let mid = clock.len() / 2;
        let mut split = Sequencer::new();
        let (f1, _) = run(&mut split, &clock[..mid], &p);
        let (f2, _) = run(&mut split, &clock[mid..], &p);

        for i in 0..mid {
            assert_eq!(f1[i], fw[i], "block 1 differs at {i}");
            assert_eq!(f2[i], fw[mid + i], "block 2 differs at {i}");
        }
    }

    #[test]
    fn first_beat_lands_on_step_zero_even_if_clock_starts_high() {
        // A fresh Clock's gate is already high at frame 0 (downbeat). That counts as a
        // rising edge from the initial low state, so the first beat plays step 0.
        let clock = beat_gate(100, 1);
        assert_eq!(clock[0], 1.0, "clock starts high");
        let pitches = [67.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut seq = Sequencer::new();
        let (freq, gate) = run(&mut seq, &clock, &params(1.0, pitches));
        approx::assert_relative_eq!(freq[10], hz(67.0), epsilon = 1e-2);
        assert_eq!(gate[10], 1.0);
    }

    #[test]
    fn spawned_sequencer_starts_before_the_first_step() {
        let mut a = Sequencer::new();
        let clock = beat_gate(100, 3);
        let pitches = [60.0, 62.0, 64.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let _ = run(&mut a, &clock, &params(3.0, pitches));
        let mut b = a.spawn();
        // The spawn is fresh: its first beat is step 0 again, not wherever `a` ended.
        let one = beat_gate(100, 1);
        let mut freq = vec![0.0f32; one.len()];
        let mut gate = vec![0.0f32; one.len()];
        {
            let outs: Vec<&mut [f32]> = vec![&mut freq[..], &mut gate[..]];
            let inputs: Vec<Option<&[f32]>> = vec![Some(&one[..])];
            let p = params(3.0, pitches);
            let mut io = Io::new(SR, one.len(), inputs, outs, &p, &[]);
            b.process(&mut io);
        }
        approx::assert_relative_eq!(freq[10], hz(60.0), epsilon = 1e-2);
    }
}
