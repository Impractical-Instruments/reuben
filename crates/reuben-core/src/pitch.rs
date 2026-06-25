//! Pitch — symbolic pitch (ADR-0008).
//!
//! Pitch is symbolic: primarily a scale degree, with a float MIDI note (60.0 = middle C)
//! always available as a 12-TET coordinate. It is resolved to a frequency in Hz by a
//! [`crate::tuning::Tuning`]; Pitch never holds a frequency itself.
//!
//! The "first sound" run uses only the float-MIDI coordinate; the scale-degree layer and
//! the tonal-context bus are filled in later.

/// A symbolic pitch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pitch {
    /// Scale degree within the active Scale (`None` until the harmony layer lands).
    pub degree: Option<i32>,
    /// Float MIDI note, the always-available 12-TET coordinate (60.0 = middle C).
    pub midi: f32,
}

impl Pitch {
    /// A pitch given directly as a float MIDI note.
    pub fn from_midi(midi: f32) -> Self {
        Self { degree: None, midi }
    }

    /// A pitch given as a scale degree, resolved against the active tonal context
    /// ([`crate::harmony::Harmony`]). The MIDI coordinate is left at 0.0 — a degree pitch
    /// carries its identity in `degree` and resolves to Hz through the context, so the
    /// degree can re-spell live on a key/scale change (ADR-0013, ADR-0015).
    pub fn from_degree(degree: i32) -> Self {
        Self {
            degree: Some(degree),
            midi: 0.0,
        }
    }
}

/// A note — a symbolic [`Pitch`] plus a velocity (ADR-0030). The atomic vocab payload of an
/// `Arg::Note`: pitch and velocity ride **one** Arg because a Message carries exactly one.
/// Velocity 0 is a note-off.
///
/// Phase 2 moves `Note` (and `Pitch`, refactored to `enum { Degree(i32), Absolute(f32) }`)
/// into the shared `vocab` module under the `ArgValue` derive; for now it lives beside `Pitch`
/// and wraps the existing struct so the [`crate::message::Arg`] spine is real.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Note {
    pub pitch: Pitch,
    pub velocity: f32,
}

impl Note {
    /// A note from a pitch and velocity.
    pub fn new(pitch: Pitch, velocity: f32) -> Self {
        Self { pitch, velocity }
    }

    /// Whether this is a note-off (velocity 0 or below).
    pub fn is_off(&self) -> bool {
        self.velocity <= 0.0
    }
}
