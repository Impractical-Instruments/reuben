//! Pitch & Note — symbolic pitch and the note vocab type.
//!
//! [`Pitch`] is symbolic: **either** a scale degree (resolved to Hz through the active
//! [`Harmony`](crate::vocab::harmony::Harmony), so it re-spells live) **or** an absolute float-MIDI
//! coordinate (60.0 = middle C, a 12-TET coordinate). Modelled as an enum so the two cannot
//! both be set or both be absent — the old `{ degree: Option<i32>, midi: f32 }` struct had
//! invalid states. A [`Tuning`](crate::tuning::Tuning) resolves an absolute pitch to
//! Hz; Pitch never holds a frequency itself.
//!
//! [`Note`] is the atomic vocab payload of an `Arg::Note`: a Pitch plus a velocity, riding
//! **one** [`Arg`](crate::message::Arg) because a Message carries exactly one.

/// A symbolic pitch — exactly one of a scale degree or an absolute MIDI coordinate.
#[derive(Debug, Clone, Copy, PartialEq, reuben_macros::ArgValue)]
pub enum Pitch {
    /// A scale degree within the active Scale. Resolves to Hz through the
    /// [`Harmony`](crate::vocab::harmony::Harmony), so it re-spells live on a key/scale change.
    Degree(i32),
    /// A float MIDI note (60.0 = middle C) — the always-available 12-TET coordinate.
    Absolute(f32),
}

impl Pitch {
    /// The load-time / pre-event default: the tonic scale **degree** (`Degree(0)`),
    /// which stays in key. The const form the `pitch` handle carries as its held-read fallback
    /// (parallel to `Harmony::DEFAULT`). A hand-written const rather than `#[default]` on the
    /// variant — `#[derive(Default)]`'s `#[default]` may only sit on a *unit* variant, and
    /// `Degree` carries an `i32`.
    pub const DEFAULT: Self = Pitch::Degree(0);

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

/// The pre-event default: the tonic degree. Hand-written because `#[default]` only
/// applies to unit variants, and [`Pitch::Degree`] carries a payload.
impl Default for Pitch {
    fn default() -> Self {
        Pitch::DEFAULT
    }
}

/// A note — a symbolic [`Pitch`] plus a velocity. The atomic vocab payload of an
/// `Arg::Note`: pitch and velocity ride **one** Arg because a Message carries exactly one.
/// Velocity 0 is a note-off.
///
/// `Default` is `{ pitch: Degree(0), velocity: 0.0 }` — the tonic degree with a
/// note-**off** velocity, so an `unpack_note` that has seen no event holds a quiet, in-key baseline:
/// a downstream envelope's gate stays closed until the first real note ([`is_off`](Note::is_off)).
#[derive(Debug, Clone, Copy, PartialEq, Default, reuben_macros::ArgValue)]
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

/// External OSC form: `/note <pitch> [velocity]`. The pitch arg's **type** picks the
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

// Self-register the flat form with the boundary's converter registry (issue #204), next to the
// `OscArg` impl it wraps — how the boundary's struct decode finds `Note` by port-type name.
crate::register_osc_form!(Note);
