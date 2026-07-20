//! Envelope — a gated ADSR **generator** that emits a linear control signal.
//!
//! Following the modular-synth split (issue #40), the envelope is a pure EG: it generates the
//! ADSR contour as **linear CV** in `[0, 1]` and emits it on its output — it does **not** apply
//! itself as a VCA. Shaping that contour into a perceptually-natural volume curve, and applying
//! it to audio, are downstream concerns: route `cv` through a curve op (e.g. `power` for an
//! exponential-style amplitude decay) and into a `mul` against the audio. Keeping the EG linear
//! makes it the flexible primitive — linear *or* any curve is a choice of downstream op.
//!
//! All inputs are Value ports, each owning its unwired default and read held (the engine
//! block-slices at each change). `gate` is edge-detected at the slice's frame 0 (the change frame),
//! so the A/R trigger stays sample-accurate; an unwired gate reads 0.
//!
//! - input 0: `gate` — > 0.5 means held; the rising/falling edge triggers A/R.
//! - input 1: `attack` — attack time in seconds.
//! - input 2: `decay` — decay time in seconds.
//! - input 3: `sustain` — sustain level 0..1.
//! - input 4: `release` — release time in seconds.
//! - output 0: `cv` (`Buffer`) — the ADSR level contour, linear `[0, 1]`.
//! - output 1: `active` (`Float`) — a held gate: `1.0` from the note-on through the whole release
//!   tail, `0.0` once the level reaches zero and the envelope is fully idle. This is the canonical
//!   **voice-liveness** source the Voicer reads to know a voice is truly finished (not merely
//!   gate-off), so a voice in its release tail is never stolen while an idle one exists. Emitted as a
//!   sparse `MsgWriter` change (one event per true↔false transition), like `euclid.gate`.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract: one declaration -> IN_/OUT_ consts + Descriptor, no drift.
crate::operator_contract!(Envelope {
    inputs:  { gate:    f32 { 0.0..=1.0, default 0.0, "", lin },
               attack:  f32 { 0.001..=5.0, default 0.01, "s", exp },
               decay:   f32 { 0.001..=5.0, default 0.1,  "s", exp },
               sustain: f32 { 0.0..=1.0,   default 0.7,  "",  lin },
               release: f32 { 0.001..=5.0, default 0.2,  "s", exp } },
    outputs: { cv:     f32_buffer,
               active: f32 { 0.0..=1.0, default 0.0, "active", lin } },
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
    /// Last value emitted on the `active` output (voice-liveness): `true` from note-on
    /// through the release tail, `false` once Idle. Persists across blocks so the held `MsgWriter`
    /// value only changes on a true↔false transition (deduped, sparse).
    active: bool,
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

        // ADSR times are `Float` inputs, read once at block rate as the held (ZOH) value via
        // `io.read` (each handle carrying its declared default) — the shape is block-rate, exactly
        // as the old params were.
        let sustain = io.read(IN_SUSTAIN).clamp(0.0, 1.0);
        let attack_step = per_sample_step(io.read(IN_ATTACK), sample_rate);
        let decay_step = per_sample_step(io.read(IN_DECAY), sample_rate) * (1.0 - sustain);
        // Base per-sample rate that would span the full [0,1] range in `release` seconds. The
        // actual Release decrement is this scaled by the level at the note-off edge (below), so
        // release lasts `release` seconds from wherever the level is — never frozen at sustain=0.
        let release_rate = per_sample_step(io.read(IN_RELEASE), sample_rate);

        // `gate` is a held Value: the engine block-slices at every gate change, so this
        // call sees one constant level — read it once and detect the edge against the flag held
        // across the previous slice. The slice's frame 0 *is* the change frame (block-absolute), so
        // the A/R trigger is sample-accurate. An unwired gate reads 0 (gate-off).
        let gate_on = io.read(IN_GATE) > 0.5;
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

        // The CV contour itself is a continuous Signal (`cv` stays `f32_buffer`): advance it
        // per-sample across the whole (sub)block.
        for i in 0..n {
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

            io.write(OUT_CV)[i] = self.level;

            // Voice-liveness: active for the whole contour incl. the release tail, idle
            // only at Stage::Idle (level == 0). Emit a sparse held change on each transition; the
            // MsgWriter dedups, so an unchanging block emits nothing and the latch holds.
            let now_active = self.stage != Stage::Idle;
            if now_active != self.active {
                io.write(OUT_ACTIVE)
                    .set(i, if now_active { 1.0 } else { 0.0 });
                self.active = now_active;
            }
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
    use crate::op_driver::OpDriver;
    use approx::assert_abs_diff_eq;

    const SR: f32 = 48_000.0;

    /// Drive `d`'s Envelope over `gate.len()` frames through the real engine, returning the emitted
    /// CV contour (the level per sample). The ADSR times are held controls (`set`); `gate` is a held
    /// Value fed as edges via `push_gate`, so attack/decay/release thread continuously across the
    /// real 128-frame blocks. `adsr` is `[attack, decay, sustain, release]`, in input-port order.
    fn run(d: &mut OpDriver, gate: &[f32], adsr: &[f32]) -> Vec<f32> {
        d.set(IN_ATTACK, adsr[0])
            .set(IN_DECAY, adsr[1])
            .set(IN_SUSTAIN, adsr[2])
            .set(IN_RELEASE, adsr[3]);
        push_gate(d, gate);
        d.render(gate.len()).output(OUT_CV).to_vec()
    }

    /// Drive the now-held-Value `gate` from a dense gate buffer: the gate is fed by edges,
    /// not a per-sample buffer. Push the first frame's level unconditionally (so a continuous render
    /// drops the latch the previous render left set; an unchanged value dedups), then a change at
    /// each frame the buffer crosses the 0.5 threshold.
    fn push_gate(d: &mut OpDriver, gate: &[f32]) {
        let Some(&first) = gate.first() else { return };
        d.push(IN_GATE, 0, first);
        let mut prev = first;
        for (i, &g) in gate.iter().enumerate().skip(1) {
            if (prev < 0.5) != (g < 0.5) {
                d.push(IN_GATE, i, g);
                prev = g;
            }
        }
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
        let gate = vec![1.0f32; n];

        let mut d = OpDriver::for_type(Envelope::new(), SR);
        let out = run(&mut d, &gate, &params);

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
    fn cv_is_the_linear_level_contour() {
        // The EG emits the raw linear level (no VCA): during attack the CV rises ~linearly
        // toward 1.0, so the midpoint of attack sits near 0.5 (a linear ramp, not a curve).
        let attack = 0.1;
        let params = vec![attack, 0.05, 1.0, 0.05];
        let n = (attack * SR) as usize;
        let gate = vec![1.0f32; n];

        let mut d = OpDriver::for_type(Envelope::new(), SR);
        let out = run(&mut d, &gate, &params);

        assert_abs_diff_eq!(out[n / 2], 0.5, epsilon = 0.02);
    }

    #[test]
    fn falls_to_zero_within_release() {
        let attack = 0.005;
        let decay = 0.005;
        let sustain = 0.6;
        let release = 0.02;
        let params = vec![attack, decay, sustain, release];

        let mut d = OpDriver::for_type(Envelope::new(), SR);

        // First render: gate held long enough to reach sustain.
        let hold_n = 4_800;
        let gate_hold = vec![1.0f32; hold_n];
        let held = run(&mut d, &gate_hold, &params);
        assert_abs_diff_eq!(held[hold_n - 1], sustain, epsilon = 1e-4);

        // Second render: gate drops; after `release` seconds the level is ~0 (the release slope is
        // locked at the note-off edge, state carried across the render boundary).
        let release_samples = (release * SR) as usize;
        let rel_n = release_samples + 1_000;
        let gate_rel = vec![0.0f32; rel_n];
        let out = run(&mut d, &gate_rel, &params);

        // Mid-release the level is still above zero (continuity across blocks).
        assert!(out[release_samples / 2] > 0.0);
        // Past the release time the level has reached zero.
        assert_abs_diff_eq!(out[rel_n - 1], 0.0, epsilon = 1e-6);
    }

    #[test]
    fn groovebox_snare_decays_to_zero_while_gate_held() {
        // Exact snare-noise envelope from instruments/groovebox.json. sustain = 0, so a held
        // gate must still fall to zero after attack+decay (a percussive "snap"), not drone.
        let attack = 0.001;
        let decay = 0.09;
        let sustain = 0.0;
        let release = 0.06;
        let params = vec![attack, decay, sustain, release];

        // Gate held high for 0.5 s — far longer than attack+decay (~0.091 s).
        let n = (0.5 * SR) as usize;
        let gate = vec![1.0f32; n];

        let mut d = OpDriver::for_type(Envelope::new(), SR);
        let out = run(&mut d, &gate, &params);

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

        let mut d = OpDriver::for_type(Envelope::new(), SR);

        // Render 1: gate high for ~62 ms — shorter than decay (90 ms), so it falls mid-decay.
        let gate_samples = (0.0625 * SR) as usize;
        let gate1 = vec![1.0f32; gate_samples];
        let held = run(&mut d, &gate1, &params);
        assert!(
            held[gate_samples - 1] > 0.05,
            "still decaying when the gate falls"
        );

        // Render 2: gate low. After the release time the level is 0 and stays there.
        let release_samples = (release * SR) as usize;
        let rel_n = release_samples + 4_800;
        let gate2 = vec![0.0f32; rel_n];
        let out = run(&mut d, &gate2, &params);

        assert_abs_diff_eq!(out[rel_n - 1], 0.0, epsilon = 1e-6);
        // And nothing lingers past the release window (the "stays open" symptom).
        assert!(
            out[release_samples + 2_400] == 0.0,
            "level still open after release — envelope stayed open"
        );
    }

    /// The F32 value carried by an `active` emit (panics on any other Arg — the contract is F32).
    fn active_val(e: &crate::message::Emit) -> f32 {
        match &e.arg {
            crate::message::Arg::F32(v) => *v,
            other => panic!("expected an F32 active flag, got {other:?}"),
        }
    }

    #[test]
    fn active_tracks_liveness_through_the_release_tail() {
        // Voice-liveness: `active` goes 1.0 at note-on and stays high through the whole
        // release tail, dropping to 0.0 only once the level reaches zero (fully idle). Sparse: one
        // emit per transition.
        let attack = 0.005;
        let decay = 0.005;
        let sustain = 0.6;
        let release = 0.02;
        let mut d = OpDriver::for_type(Envelope::new(), SR);
        d.set(IN_ATTACK, attack)
            .set(IN_DECAY, decay)
            .set(IN_SUSTAIN, sustain)
            .set(IN_RELEASE, release);

        // Note-on held to sustain: exactly one active=1.0 at the downbeat.
        let hold_n = 4_800;
        push_gate(&mut d, &vec![1.0f32; hold_n]);
        let on = d.render(hold_n).emits().to_vec();
        assert_eq!(on.len(), 1, "one transition: idle -> active");
        assert_eq!(on[0].frame, 0);
        assert_abs_diff_eq!(active_val(&on[0]), 1.0);

        // Gate drops: still active through the release tail, then exactly one active=0.0 once idle.
        let release_samples = (release * SR) as usize;
        let rel_n = release_samples + 2_400;
        push_gate(&mut d, &vec![0.0f32; rel_n]);
        let off = d.render(rel_n).emits().to_vec();
        assert_eq!(off.len(), 1, "one transition: active -> idle");
        assert_abs_diff_eq!(active_val(&off[0]), 0.0);
        // The idle transition lands inside the release window, not at frame 0 (the tail is alive).
        assert!(off[0].frame > 0 && off[0].frame <= release_samples + 64);
    }

    #[test]
    fn retrigger_during_release_resumes_attack_from_current_level() {
        // Fast repeated notes on a mono line: the gate rises again while the envelope is
        // mid-Release. The retrigger must flip the stage back to Attack and ramp from the level
        // *at that instant* — never reset to 0 (an audible click) — so the top arrives in less
        // than a full attack time. And `active` (voice-liveness) must emit nothing at
        // the retrigger: the envelope never reached Idle, so the held MsgWriter value never
        // transitions — the Voicer keeps seeing one continuously-live voice.
        let attack = 0.01; // 480 samples
        let decay = 0.01; // 480 samples
        let sustain = 0.6;
        let release = 0.04; // 1920 samples
        let mut d = OpDriver::for_type(Envelope::new(), SR);
        d.set(IN_ATTACK, attack)
            .set(IN_DECAY, decay)
            .set(IN_SUSTAIN, sustain)
            .set(IN_RELEASE, release);

        // One render, gate edges pushed directly: note-on at 0; note-off at 4800 (well past
        // attack+decay, level == sustain 0.6); retrigger at 5760 — half-way through the
        // 1920-sample release, so the level is ~0.3 at that instant.
        let retrig = 5_760;
        d.push(IN_GATE, 0, 1.0)
            .push(IN_GATE, 4_800, 0.0)
            .push(IN_GATE, retrig, 1.0);
        let out = d.render(12_000).output(OUT_CV).to_vec();

        // Mid-release just before the retrigger: half-way down from sustain.
        assert_abs_diff_eq!(out[retrig - 1], 0.3, epsilon = 0.02);

        // From the retrigger the level only rises until it tops out — no reset, no dip toward 0.
        assert!(
            out[retrig] >= out[retrig - 1] - 1e-4,
            "level reset at the retrigger frame: {} -> {}",
            out[retrig - 1],
            out[retrig]
        );
        let top = retrig
            + out[retrig..]
                .iter()
                .position(|&s| s >= 0.99)
                .expect("retriggered attack reaches the top");
        for i in retrig..top {
            assert!(
                out[i + 1] >= out[i] - 1e-4,
                "level dipped during the retriggered attack at {i}: {} -> {}",
                out[i],
                out[i + 1]
            );
        }

        // Resuming from ~0.3 the attack tops out in ~(1-0.3)*480 ≈ 336 samples — well inside 400,
        // faster than the 480-sample full attack (a reset-to-0 regression only reads ~0.83 here).
        let peak = out[retrig..=retrig + 400]
            .iter()
            .cloned()
            .fold(0.0f32, f32::max);
        assert_abs_diff_eq!(peak, 1.0, epsilon = 0.02);

        // Sparse `active`: exactly one emit across the whole run — the note-on transition at
        // frame 0. No active=0.0 (the release never completed) and no second active=1.0 (the
        // retrigger is not a liveness transition; the latch dedups).
        let emits = d.emits();
        assert_eq!(
            emits.len(),
            1,
            "expected exactly one active transition, got {emits:?}"
        );
        assert_eq!(emits[0].frame, 0);
        assert_abs_diff_eq!(active_val(&emits[0]), 1.0);
    }

    #[test]
    fn gate_never_on_is_silent() {
        let params = vec![0.01, 0.1, 0.7, 0.2];
        let n = 1_024;
        let gate = vec![0.0f32; n];

        let mut d = OpDriver::for_type(Envelope::new(), SR);
        let out = run(&mut d, &gate, &params);

        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn unconnected_gate_is_silent() {
        // Nothing wired: gate (and the ADSR inputs) read as their unwired defaults. A gate that
        // never goes high holds the envelope Idle at 0 regardless of the ADSR times.
        let mut d = OpDriver::for_type(Envelope::new(), SR);
        let out = d.render(256).output(OUT_CV).to_vec();
        assert!(out.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn cv_reaches_unity_ceiling() {
        // Instant attack/decay with sustain = 1.0 drives the level to its 1.0 ceiling almost
        // immediately and holds there — the CV is bounded to [0, 1].
        let params = vec![0.0005, 0.0005, 1.0, 0.05];
        let n = 2_048;
        let gate = vec![1.0f32; n];

        let mut d = OpDriver::for_type(Envelope::new(), SR);
        let out = run(&mut d, &gate, &params);

        assert_abs_diff_eq!(out[n - 1], 1.0, epsilon = 1e-4);
        assert!(out.iter().all(|&s| (0.0..=1.0 + 1e-6).contains(&s)));
    }
}
