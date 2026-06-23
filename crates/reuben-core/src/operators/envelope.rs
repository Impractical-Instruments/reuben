//! Envelope — gated ADSR applied as a VCA.
//!
//! Ports/params are FROZEN (Stage A). DSP body is filled test-first in Stage B.
//!
//! - input 0: `audio` (Signal) — the signal to shape.
//! - input 1: `gate` (Signal) — > 0.5 means held; the rising/falling edge triggers A/R.
//! - output 0: `audio` (Signal) — `audio * env`.
//! - params 0..3: `attack`, `decay`, `sustain`, `release`.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract (ADR-0025): one declaration -> IN_/OUT_/P_ consts + Descriptor, no drift.
crate::operator_contract!(Envelope {
    inputs:  { audio: signal, gate: signal },
    outputs: { audio: signal },
    params:  { attack:  { 0.001..=5.0, default 0.01, "s", exp },
               decay:   { 0.001..=5.0, default 0.1,  "s", exp },
               sustain: { 0.0..=1.0,   default 0.7,  "",  lin },
               release: { 0.001..=5.0, default 0.2,  "s", exp } },
});

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
    /// Per-sample decrement for the in-progress Release, fixed at the note-off edge from the
    /// level *at that instant* so the level always falls to 0 in `release` seconds — regardless
    /// of `sustain`, and correct when the gate falls mid-decay. Persists across blocks.
    release_step: f32,
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
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        let n = io.frames();
        let sample_rate = io.sample_rate();

        let sustain = io.param(P_SUSTAIN).clamp(0.0, 1.0);
        let attack_step = per_sample_step(io.param(P_ATTACK), sample_rate);
        let decay_step = per_sample_step(io.param(P_DECAY), sample_rate) * (1.0 - sustain);
        // Base per-sample rate that would span the full [0,1] range in `release` seconds. The
        // actual Release decrement is this scaled by the level at the note-off edge (below), so
        // release lasts `release` seconds from wherever the level is — never frozen at sustain=0.
        let release_rate = per_sample_step(io.param(P_RELEASE), sample_rate);

        // Read each input sample with a short-lived borrow that ends before the output
        // write, so `process` stays allocation-free. Unconnected inputs read as
        // silence / gate-off.
        for i in 0..n {
            let gate_on = io.input(IN_GATE).map_or(0.0, |s| s[i]) > 0.5;
            let audio_in = io.input(IN_AUDIO).map_or(0.0, |s| s[i]);

            // Edge detection against the previous sample's held flag.
            if gate_on && !self.held {
                self.stage = Stage::Attack;
            } else if !gate_on && self.held {
                self.stage = Stage::Release;
                // Lock the release slope to the current level: fall to 0 over `release` seconds
                // from here. For a note held to sustain this equals the old sustain-scaled rate;
                // for sustain=0 or a release mid-decay it still terminates instead of sticking.
                self.release_step = self.level * release_rate;
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
                    self.level -= self.release_step;
                    if self.level <= 0.0 {
                        self.level = 0.0;
                        self.stage = Stage::Idle;
                    }
                }
            }

            io.output(OUT_AUDIO)[i] = audio_in * self.level;
        }
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Envelope);

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
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            let inputs: Vec<Option<&[f32]>> = vec![Some(audio), Some(gate)];
            let mut io = Io::new(SR, n, inputs, outs, params, &[]);
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
    fn groovebox_snare_decays_to_zero_while_gate_held() {
        // Exact snare-noise envelope from instruments/groovebox.json. sustain = 0, so a held
        // gate must still fall silent after attack+decay (a percussive "snap"), not drone.
        let attack = 0.001;
        let decay = 0.09;
        let sustain = 0.0;
        let release = 0.06;
        let params = vec![attack, decay, sustain, release];

        // Gate held high for 0.5 s — far longer than attack+decay (~0.091 s).
        let n = (0.5 * SR) as usize;
        let audio = vec![1.0f32; n];
        let gate = vec![1.0f32; n];

        let mut env = Envelope::new();
        let out = run(&mut env, &audio, &gate, &params);

        // By the end of attack+decay the level has reached sustain (0.0)…
        let settle = ((attack + decay) * SR) as usize + 64;
        assert_abs_diff_eq!(out[settle], 0.0, epsilon = 1e-6);
        // …and stays there for the rest of the held gate (no drone / stuck-open).
        assert_abs_diff_eq!(out[n - 1], 0.0, epsilon = 1e-6);
    }

    #[test]
    fn short_gate_with_zero_sustain_releases_to_zero() {
        // The groovebox snare bug: sustain = 0 and the gate falls *before* decay completes, so
        // Release starts mid-decay (level well above 0). The level must still reach 0 within the
        // release time — not freeze at the note-off level (which droned the snare noise).
        let attack = 0.001;
        let decay = 0.09;
        let sustain = 0.0;
        let release = 0.06;
        let params = vec![attack, decay, sustain, release];

        let mut env = Envelope::new();

        // Block 1: gate high for ~62 ms — shorter than decay (90 ms), so it falls mid-decay.
        let gate_samples = (0.0625 * SR) as usize;
        let audio1 = vec![1.0f32; gate_samples];
        let gate1 = vec![1.0f32; gate_samples];
        let held = run(&mut env, &audio1, &gate1, &params);
        assert!(
            held[gate_samples - 1] > 0.05,
            "still decaying when the gate falls"
        );

        // Block 2: gate low. After the release time the level is 0 and stays there.
        let release_samples = (release * SR) as usize;
        let rel_n = release_samples + 4_800;
        let audio2 = vec![1.0f32; rel_n];
        let gate2 = vec![0.0f32; rel_n];
        let out = run(&mut env, &audio2, &gate2, &params);

        assert_abs_diff_eq!(out[rel_n - 1], 0.0, epsilon = 1e-6);
        // And nothing lingers past the release window (the "stays open" symptom).
        assert!(
            out[release_samples + 2_400] == 0.0,
            "noise still audible after release — envelope stayed open"
        );
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
            let outs: Vec<&mut [f32]> = vec![&mut out[..]];
            // No audio, no gate.
            let inputs: Vec<Option<&[f32]>> = vec![None, None];
            let mut io = Io::new(SR, n, inputs, outs, &params, &[]);
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
