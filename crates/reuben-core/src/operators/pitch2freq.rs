//! `pitch2freq` — the wire-exposed form of `Harmony::hz`: a symbolic pitch → a frequency.
//!
//! The single stage that **exits** the symbolic pitch domain into raw Hz. Everything
//! upstream keeps pitch symbolic — `snap`/`chord` are `Note`→`Note` re-spellings; the Voicer alone
//! used to turn a held pitch into an output frequency, welded inside the monolith. This operator
//! de-traps that lowering so a top-level mono voice (`unpack_note` → `pitch2freq` → osc/env) can
//! resolve its sequencer-driven pitch without a Voicer.
//!
//! - input 0: `pitch` ([`Pitch`](crate::vocab::pitch::Pitch), held) — the symbolic pitch to resolve;
//!   a [`Degree`](crate::vocab::pitch::Pitch::Degree) resolves through the live scale + tuning (so it
//!   re-spells on `/key`/`/mode`), an [`Absolute`](crate::vocab::pitch::Pitch::Absolute) through 12-TET.
//!   Default [`Pitch::DEFAULT`] (tonic `Degree(0)`).
//! - input 1: `harmony` ([`Harmony`](crate::vocab::harmony::Harmony), held) — the tonal frame to
//!   resolve against. Default [`Harmony::DEFAULT`] (C major, 12-TET).
//! - output 0: `freq` (`f32`, held **Value**) — the resolved frequency in Hz. Piecewise-constant: it
//!   changes only when a new pitch arrives or `Harmony` re-spells, so it is emitted as a held Value
//!   (a sparse change), not a per-sample Signal. An oscillator's `freq` Signal input accepts it
//!   through the standard ZOH bridge; portamento is a downstream `m2s` in Glide mode.
//!
//! see rules: signal-time-dsp
//!
//! Pitch-only by design: velocity and gate never enter it — in the unbundling chain
//! they flow on the separate `velocity` wire out of `unpack_note`. Folding them in would re-bundle
//! what the map dissolved and give the operator a second responsibility. `process` is a pure lookup
//! (`freq = harmony.hz(pitch)`) on `Copy` inputs — stateless, allocation-free, hot-path-trivial.

use crate::descriptor::Descriptor;
use crate::operator::{Io, Operator};

// Single-source contract: one declaration -> IN_/OUT_ consts + Descriptor, no drift. `pitch` and
// `harmony` are held vocab-type Values; `freq` is a held `f32` Value (the frequency-shaped range so
// an unwired downstream materializes a sane 440 Hz default, mirroring the oscillator's `freq`).
crate::operator_contract!(Pitch2Freq {
    type_name: "pitch2freq",
    inputs: { pitch: pitch, harmony: harmony },
    outputs: { freq: f32 { 20.0..=20000.0, default 440.0, "Hz", exp } },
});

#[derive(Default)]
pub struct Pitch2Freq;

impl Pitch2Freq {
    pub fn new() -> Self {
        Self
    }
}

impl Operator for Pitch2Freq {
    fn descriptor() -> Descriptor {
        Self::contract()
    }

    fn process(&mut self, io: &mut Io) {
        // Both inputs are held Values: the engine block-slices at every pitch/harmony change, so this
        // call reads one constant pitch and one constant `Harmony`, and `freq` is a single value for
        // the (sub)block. Emit it at the slice's frame 0 — a mid-block change arrives as the next
        // slice's frame 0 (the change frame), so the emitted Value stays sample-accurate. The
        // deduping `MsgWriter` keeps steady state sparse (one baseline per block, no re-fire when the
        // resolved frequency is unchanged).
        let pitch = io.read(IN_PITCH);
        let harmony = io.read(IN_HARMONY);
        io.write(OUT_FREQ).set(0, harmony.hz(pitch));
    }

    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Self::new())
    }
}

crate::register_operator!(Pitch2Freq);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Arg, Emit};
    use crate::op_driver::OpDriver;
    use crate::vocab::harmony::Harmony;
    use crate::vocab::pitch::Pitch;

    const SR: f32 = 48_000.0;

    /// The Hz an emit on the `freq` port carries.
    fn hz_of(e: &Emit) -> f32 {
        match &e.arg {
            Arg::F32(f) => *f,
            other => panic!("expected an f32 on the freq port, got {other:?}"),
        }
    }

    /// Every `freq` emit `(frame, Hz)` across the render, in emit order.
    fn freqs(d: &OpDriver) -> Vec<(usize, f32)> {
        d.emits()
            .iter()
            .filter(|e| e.port == OUT_FREQ.index())
            .map(|e| (e.frame, hz_of(e)))
            .collect()
    }

    /// Resolve `pitch` against `harmony` through the real engine over one block; return the single
    /// held `freq` value the operator emits (its frame-0 baseline).
    fn resolve(pitch: Pitch, harmony: Harmony) -> f32 {
        let mut d = OpDriver::for_type(Pitch2Freq::new(), SR);
        d.set(IN_PITCH, pitch).set(IN_HARMONY, harmony);
        d.render(128);
        let out = freqs(&d);
        assert_eq!(out.len(), 1, "one held baseline per block, got {out:?}");
        assert_eq!(out[0].0, 0, "the baseline lands at frame 0");
        out[0].1
    }

    // A `Degree` resolves through the scale + tuning of the held `Harmony` — the same lowering the
    // Voicer did. C major: degrees 0/2/4 are C4/E4/G4 (matches the vocab-level `Harmony::hz` oracle).
    #[test]
    fn resolves_a_degree_through_scale_and_tuning() {
        let c = Harmony::default();
        approx::assert_relative_eq!(resolve(Pitch::from_degree(0), c), 261.63, epsilon = 0.05);
        approx::assert_relative_eq!(resolve(Pitch::from_degree(2), c), 329.63, epsilon = 0.05);
        approx::assert_relative_eq!(resolve(Pitch::from_degree(4), c), 392.00, epsilon = 0.05);
    }

    // An `Absolute` pitch passes straight through 12-TET, ignoring the scale: MIDI 69 = A4 = 440 Hz.
    #[test]
    fn resolves_an_absolute_pitch_through_12tet() {
        approx::assert_relative_eq!(
            resolve(Pitch::from_midi(69.0), Harmony::default()),
            440.0,
            epsilon = 0.05
        );
    }

    // The `harmony` port is genuinely read: the *same* degree resolves to a different frequency when
    // the held `Harmony` re-spells it. Shifting the root C4→D4 (60→62) moves the tonic degree from
    // 261.63 Hz to 293.66 Hz — proving live re-spelling, and that the port isn't ignored/mis-indexed.
    #[test]
    fn re_spells_a_degree_when_harmony_changes() {
        let d_major = Harmony {
            root: 62,
            ..Harmony::default()
        };
        approx::assert_relative_eq!(
            resolve(Pitch::from_degree(0), d_major),
            293.66,
            epsilon = 0.05
        );
    }

    // Both inputs default sensibly: an unwired `pitch2freq` resolves to the tonic
    // frequency rather than faulting — `Pitch::DEFAULT` (`Degree(0)`) through `Harmony::DEFAULT`
    // (C major) is C4 = 261.63 Hz.
    #[test]
    fn unwired_resolves_to_the_tonic_frequency() {
        let mut d = OpDriver::for_type(Pitch2Freq::new(), SR);
        d.render(128); // nothing set — both ports fall back to their declared defaults
        let out = freqs(&d);
        assert_eq!(out.len(), 1);
        approx::assert_relative_eq!(out[0].1, 261.63, epsilon = 0.05);
    }

    // The operator is a pure lookup — a `spawn`ed copy resolves identically and independently, with
    // no state carried from the original.
    #[test]
    fn a_spawned_copy_resolves_independently() {
        let mut d = OpDriver::for_type(Pitch2Freq::new(), SR);
        d.set(IN_PITCH, Pitch::from_degree(4))
            .set(IN_HARMONY, Harmony::default());
        d.render(128);

        let mut fresh = d.spawn();
        fresh.set(IN_PITCH, Pitch::from_midi(69.0));
        fresh.render(128);
        let out = freqs(&fresh);
        assert_eq!(out.len(), 1);
        approx::assert_relative_eq!(out[0].1, 440.0, epsilon = 0.05);
    }
}
