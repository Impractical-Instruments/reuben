//! Envelope — gated ADSR applied as a VCA.
//!
//! Ports/params are FROZEN (Stage A). DSP body is filled test-first in Stage B.
//!
//! - input 0: `audio` (Signal) — the signal to shape.
//! - input 1: `gate` (Signal) — > 0.5 means held; the rising/falling edge triggers A/R.
//! - output 0: `audio` (Signal) — `audio * env`.
//! - params 0..3: `attack`, `decay`, `sustain`, `release`.

use crate::descriptor::{Curve, Descriptor, LaneRule, ParamMeta, Port};
use crate::operator::{Io, Operator};

pub const IN_AUDIO: usize = 0;
pub const IN_GATE: usize = 1;
pub const OUT_AUDIO: usize = 0;
pub const P_ATTACK: usize = 0;
pub const P_DECAY: usize = 1;
pub const P_SUSTAIN: usize = 2;
pub const P_RELEASE: usize = 3;

/// Which segment of the ADSR contour the envelope is currently traversing.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Stage {
    /// Gate is off and the level has reached zero.
    #[default]
    Idle,
    /// Gate just went high: ramp the level up toward 1.0.
    Attack,
    /// Attack finished: ramp the level down toward `sustain`.
    Decay,
    /// Gate held and decay done: hold at `sustain`.
    Sustain,
    /// Gate went low: ramp the level down toward 0.0.
    Release,
}

#[derive(Default)]
pub struct Envelope {
    /// Current envelope level [0, 1].
    level: f32,
    /// Whether the gate was held on the previous sample.
    held: bool,
    /// Current ADSR segment.
    stage: Stage,
}

impl Envelope {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Convert a time in seconds into a per-sample linear increment that traverses a
/// unit distance. A non-positive time collapses to "instant" (one sample).
fn per_sample_step(seconds: f32, sample_rate: f32) -> f32 {
    let samples = seconds * sample_rate;
    if samples <= 1.0 {
        1.0
    } else {
        1.0 / samples
    }
}

impl Operator for Envelope {
    fn descriptor() -> Descriptor {
        fn time(name: &'static str, default: f32) -> ParamMeta {
            ParamMeta {
                name,
                min: 0.001,
                max: 5.0,
                default,
                unit: "s",
                curve: Curve::Exponential,
            }
        }
        Descriptor {
            type_name: "envelope",
            inputs: vec![Port::signal("audio"), Port::signal("gate")],
            outputs: vec![Port::signal("audio")],
            params: vec![
                time("attack", 0.01),
                time("decay", 0.1),
                ParamMeta {
                    name: "sustain",
                    min: 0.0,
                    max: 1.0,
                    default: 0.7,
                    unit: "",
                    curve: Curve::Linear,
                },
                time("release", 0.2),
            ],
            lanes: LaneRule::Inherit,
        }
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        let sustain = io.param(P_SUSTAIN).clamp(0.0, 1.0);
        let attack_step = per_sample_step(io.param(P_ATTACK), sample_rate);
        let decay_step = per_sample_step(io.param(P_DECAY), sample_rate) * (1.0 - sustain);
        let release_step = per_sample_step(io.param(P_RELEASE), sample_rate) * sustain.max(1e-6);

        // Snapshot the inputs so we can take a mutable borrow of the output. Both
        // are at most `frames` long; an unconnected input reads as silence/gate-off.
        let audio: Vec<f32> = io
            .input(IN_AUDIO)
            .map(|s| s.to_vec())
            .unwrap_or_else(|| vec![0.0; n]);
        let gate: Vec<f32> = io
            .input(IN_GATE)
            .map(|s| s.to_vec())
            .unwrap_or_else(|| vec![0.0; n]);

        let out = io.output(OUT_AUDIO);

        for i in 0..n {
            let gate_on = gate[i] > 0.5;

            // Edge detection against the previous sample's held flag.
            if gate_on && !self.held {
                self.stage = Stage::Attack;
            } else if !gate_on && self.held {
                self.stage = Stage::Release;
            }
            self.held = gate_on;

            match self.stage {
                Stage::Idle => {
                    self.level = 0.0;
                }
                Stage::Attack => {
                    self.level += attack_step;
                    if self.level >= 1.0 {
                        self.level = 1.0;
                        self.stage = Stage::Decay;
                    }
                }
                Stage::Decay => {
                    self.level -= decay_step;
                    if self.level <= sustain {
                        self.level = sustain;
                        self.stage = Stage::Sustain;
                    }
                }
                Stage::Sustain => {
                    self.level = sustain;
                }
                Stage::Release => {
                    self.level -= release_step;
                    if self.level <= 0.0 {
                        self.level = 0.0;
                        self.stage = Stage::Idle;
                    }
                }
            }

            out[i] = audio[i] * self.level;
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::Io;
    use approx::assert_abs_diff_eq;

    const SR: f32 = 48_000.0;

    /// Run `env` over a single block of `n` frames with the given audio and gate
    /// inputs and ADSR params, returning the shaped output.
    fn run(env: &mut Envelope, audio: &[f32], gate: &[f32], params: &[f32]) -> Vec<f32> {
        let n = audio.len();
        let mut out = vec![0.0f32; n];
        {
            let mut outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let inputs: Vec<Option<&[f32]>> = vec![Some(audio), Some(gate)];
            let mut io = Io::new(SR, n, &inputs, &mut outs, params, &[]);
            env.process(&mut io);
        }
        out
    }

    #[test]
    fn rises_to_one_then_settles_to_sustain() {
        let attack = 0.01;
        let decay = 0.02;
        let sustain = 0.5;
        let release = 0.05;
        let params = vec![attack, decay, sustain, release];

        // Long enough to finish attack + decay and dwell on sustain.
        let n = ((attack + decay) * SR) as usize + 4_800;
        let audio = vec![1.0f32; n];
        let gate = vec![1.0f32; n];

        let mut env = Envelope::new();
        let out = run(&mut env, &audio, &gate, &params);

        // Peak (≈1.0) should occur right around the end of the attack stage.
        let attack_samples = (attack * SR) as usize;
        let peak = out[attack_samples - 1..attack_samples + 2]
            .iter()
            .cloned()
            .fold(0.0f32, f32::max);
        assert_abs_diff_eq!(peak, 1.0, epsilon = 0.02);

        // After attack+decay the level settles to sustain.
        assert_abs_diff_eq!(out[n - 1], sustain, epsilon = 1e-4);
    }

    #[test]
    fn falls_to_zero_within_release() {
        let attack = 0.005;
        let decay = 0.005;
        let sustain = 0.6;
        let release = 0.02;
        let params = vec![attack, decay, sustain, release];

        let mut env = Envelope::new();

        // First block: gate held long enough to reach sustain.
        let hold_n = 4_800;
        let audio_hold = vec![1.0f32; hold_n];
        let gate_hold = vec![1.0f32; hold_n];
        let held = run(&mut env, &audio_hold, &gate_hold, &params);
        assert_abs_diff_eq!(held[hold_n - 1], sustain, epsilon = 1e-4);

        // Second block: gate drops; after `release` seconds the level is ~0.
        let release_samples = (release * SR) as usize;
        let rel_n = release_samples + 1_000;
        let audio_rel = vec![1.0f32; rel_n];
        let gate_rel = vec![0.0f32; rel_n];
        let out = run(&mut env, &audio_rel, &gate_rel, &params);

        // Mid-release the level is still above zero (continuity across blocks).
        assert!(out[release_samples / 2] > 0.0);
        // Past the release time the level has reached zero.
        assert_abs_diff_eq!(out[rel_n - 1], 0.0, epsilon = 1e-6);
    }

    #[test]
    fn gate_never_on_is_silent() {
        let params = vec![0.01, 0.1, 0.7, 0.2];
        let n = 1_024;
        let audio = vec![1.0f32; n];
        let gate = vec![0.0f32; n];

        let mut env = Envelope::new();
        let out = run(&mut env, &audio, &gate, &params);

        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn unconnected_inputs_are_silent() {
        let params = vec![0.01, 0.1, 0.7, 0.2];
        let n = 256;
        let mut out = vec![0.0f32; n];
        {
            let mut outs: Vec<&mut [f32]> = vec![&mut out[..]];
            // No audio, no gate.
            let inputs: Vec<Option<&[f32]>> = vec![None, None];
            let mut io = Io::new(SR, n, &inputs, &mut outs, &params, &[]);
            Envelope::new().process(&mut io);
        }
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn output_is_input_times_env() {
        // Instant attack/decay with sustain = 1.0 makes the envelope reach 1.0
        // almost immediately, so the steady-state output equals the input scaled.
        let params = vec![0.0005, 0.0005, 1.0, 0.05];
        let n = 2_048;
        let amp = 0.5;
        let audio = vec![amp; n];
        let gate = vec![1.0f32; n];

        let mut env = Envelope::new();
        let out = run(&mut env, &audio, &gate, &params);

        // Once the envelope has reached its ceiling, output == input * 1.0 == amp.
        assert_abs_diff_eq!(out[n - 1], amp, epsilon = 1e-4);

        // And generally out[i] == audio[i] * level, bounded by the input amplitude.
        assert!(out.iter().all(|&s| (0.0..=amp + 1e-6).contains(&s)));
    }
}
