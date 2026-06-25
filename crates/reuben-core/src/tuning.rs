//! Tuning — resolves symbolic [`Pitch`] to frequency in Hz (ADR-0008).
//!
//! 12-TET is just the default Tuning. Scala `.scl`/`.kbm` import and the tonal-context
//! bus (live retuning while notes sound) land later; the trait is the seam.

use crate::pitch::Pitch;

/// Resolves a symbolic Pitch to a concrete frequency.
pub trait Tuning: Send {
    fn hz(&self, pitch: Pitch) -> f32;
}

/// Standard 12-tone equal temperament, A4 = `ref_hz`.
#[derive(Debug, Clone, Copy)]
pub struct TwelveTet {
    pub ref_hz: f32,
    pub ref_midi: f32,
}

impl Default for TwelveTet {
    fn default() -> Self {
        Self {
            ref_hz: 440.0,
            ref_midi: 69.0,
        }
    }
}

impl Tuning for TwelveTet {
    fn hz(&self, pitch: Pitch) -> f32 {
        // The tuning-only layer resolves an absolute MIDI coordinate directly. A bare degree
        // with no Harmony to resolve it falls back to a chromatic reading from middle C (60);
        // real degree resolution goes through `Harmony::hz` (ADR-0008, ADR-0030).
        let midi = match pitch {
            Pitch::Absolute(m) => m,
            Pitch::Degree(d) => 60.0 + d as f32,
        };
        self.ref_hz * 2.0_f32.powf((midi - self.ref_midi) / 12.0)
    }
}
