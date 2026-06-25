//! Pitch & Note — symbolic pitch and the note vocab type (ADR-0008, ADR-0030).
//!
//! [`Pitch`] is symbolic: **either** a scale degree (resolved to Hz through the active
//! [`Harmony`](crate::harmony::Harmony), so it re-spells live) **or** an absolute float-MIDI
//! coordinate (60.0 = middle C, a 12-TET coordinate). Modelled as an enum so the two cannot
//! both be set or both be absent — the old `{ degree: Option<i32>, midi: f32 }` struct had
//! invalid states (ADR-0030). A [`Tuning`](crate::tuning::Tuning) resolves an absolute pitch to
//! Hz; Pitch never holds a frequency itself.
//!
//! [`Note`] is the atomic vocab payload of an `Arg::Note`: a Pitch plus a velocity, riding
//! **one** [`Arg`](crate::message::Arg) because a Message carries exactly one.

/// A symbolic pitch — exactly one of a scale degree or an absolute MIDI coordinate (ADR-0030).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Pitch {
    /// A scale degree within the active Scale. Resolves to Hz through the
    /// [`Harmony`](crate::harmony::Harmony), so it re-spells live on a key/scale change.
    Degree(i32),
    /// A float MIDI note (60.0 = middle C) — the always-available 12-TET coordinate.
    Absolute(f32),
}

impl Pitch {
    /// A pitch given directly as a float MIDI note.
    pub fn from_midi(midi: f32) -> Self {
        Pitch::Absolute(midi)
    }

    /// A pitch given as a scale degree, resolved against the active tonal context.
    pub fn from_degree(degree: i32) -> Self {
        Pitch::Degree(degree)
    }

    /// The scale degree, if this is a [`Degree`](Pitch::Degree) pitch.
    pub fn degree(self) -> Option<i32> {
        match self {
            Pitch::Degree(d) => Some(d),
            Pitch::Absolute(_) => None,
        }
    }

    /// The absolute MIDI coordinate, if this is an [`Absolute`](Pitch::Absolute) pitch.
    pub fn midi(self) -> Option<f32> {
        match self {
            Pitch::Absolute(m) => Some(m),
            Pitch::Degree(_) => None,
        }
    }
}

/// A note — a symbolic [`Pitch`] plus a velocity (ADR-0030). The atomic vocab payload of an
/// `Arg::Note`: pitch and velocity ride **one** Arg because a Message carries exactly one.
/// Velocity 0 is a note-off.
#[derive(Debug, Clone, Copy, PartialEq, reuben_macros::ArgValue)]
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

/// External OSC form: `/note <pitch> [velocity]` (ADR-0030). The pitch arg's **type** picks the
/// [`Pitch`] case — an integer is a scale [`Degree`](Pitch::Degree), a float an
/// [`Absolute`](Pitch::Absolute) MIDI coordinate (so a controller sending float MIDI notes lands
/// as absolute, the historical decode convention). Velocity is the optional second arg, defaulting
/// to 1.0 (a full-velocity note-on) when omitted.
impl crate::message::OscArg for Note {
    fn from_osc(args: &[crate::message::Arg]) -> Option<Self> {
        use crate::message::Arg;
        let pitch = match args.first()? {
            Arg::I32(d) => Pitch::Degree(*d),
            Arg::F32(m) => Pitch::Absolute(*m),
            _ => return None,
        };
        let velocity = args.get(1).and_then(|a| a.as_f32()).unwrap_or(1.0);
        Some(Note::new(pitch, velocity))
    }

    fn to_osc(&self, out: &mut Vec<crate::message::Arg>) {
        use crate::message::Arg;
        match self.pitch {
            Pitch::Degree(d) => out.push(Arg::I32(d)),
            Pitch::Absolute(m) => out.push(Arg::F32(m)),
        }
        out.push(Arg::F32(self.velocity));
    }
}
