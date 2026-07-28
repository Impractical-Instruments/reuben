//! vocab — the shared concrete types that ride the central [`Arg`](crate::message::Arg).
//! see rules: composition-operators
//!
//! Each type carries `#[derive(ArgValue)]` (`crate::ArgValue`), which generates its `Arg`
//! integration — `From`/`TryFrom` — plus, for enums, the Enum-over-OSC table (`VARIANTS` /
//! `from_symbol` / `resolve_arg` / `enum_meta`).
//!
//! Adding a domain type = define it here (or beside its logic), derive `ArgValue`, and add one
//! variant to [`Arg`](crate::message::Arg). A struct type that should cross the OSC boundary
//! also hand-implements [`OscArg`](crate::message::OscArg) (its flat multi-arg form,
//! `from_osc`/`to_osc`) and self-registers the converter beside that impl with
//! `crate::register_osc_form!` — [`Note`] does; [`Harmony`] deliberately does
//! neither (the boundary opt-out; it has no external wire form yet).
//!
//! Types live next to their behavior — [`Harmony`] and its resolver in the [`harmony`]
//! submodule, [`Pitch`]/[`Note`] in [`pitch`] — and are re-exported here so a
//! consumer reaches the whole vocabulary through one path (`crate::vocab::*`).

pub mod harmony;
pub mod pitch;

pub use harmony::{Chord, ChordTag, Harmony, ScaleField, SnapDir, SnapPolicy, SnapTarget};
pub use pitch::{Note, Pitch};

/// How a sequencer step drives its output (the sequencer's `gate_mode`). A shared *vocab* enum
/// (`Arg::Enum`): emit a pitched **degree** per step, or a bare **gate** trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, reuben_macros::ArgValue)]
pub enum GateMode {
    /// Emit a degree (pitched) per active step.
    #[default]
    Degree,
    /// Emit a bare gate/trigger per active step.
    Gate,
}

/// The state-variable filter's output tap (the filter's `mode`). A shared *vocab* enum
/// (`Arg::Enum`): the TPT SVF computes all three responses from one integrator state, so the
/// mode selects which is read. `Lp` is the default (bit-identical to the original lowpass).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, reuben_macros::ArgValue)]
pub enum FilterMode {
    /// Low-pass (`v2`).
    #[default]
    Lp,
    /// High-pass (`x - k·bp - lp`).
    Hp,
    /// Band-pass (`v1`).
    Bp,
}

/// An oscillator's waveform (the oscillator's `waveform`). A shared *vocab* enum
/// (`Arg::Enum`): the band-limited shape generated each sample. `Sine` is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, reuben_macros::ArgValue)]
pub enum Waveform {
    /// Pure sine.
    #[default]
    Sine,
    /// PolyBLEP sawtooth.
    Saw,
}

/// A granulator grain's amplitude envelope (the granulator's `window`). A shared *vocab* enum
/// (`Arg::Enum`): the shape multiplied over each grain across its lifetime, evaluated at the
/// grain's normalized phase in [0, 1). `Hann` (raised cosine, click-free) is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, reuben_macros::ArgValue)]
pub enum GrainWindow {
    /// Raised cosine `0.5·(1 − cos(2π·x))` — zero at both edges, peak mid-grain. Click-free.
    #[default]
    Hann,
    /// Linear up-down ramp `1 − |2x − 1|` — zero at edges, peak mid-grain. Sharper than Hann.
    Triangle,
    /// Flat-top with cosine tapers (25% each side) — sustains the grain body, fades the edges.
    Tukey,
    /// Rectangular `1.0` — no fade. Verbatim playback of the grain body; clicks at grain edges.
    Rect,
}

/// How `m2s` fills the dense per-sample gaps between sparse messages (its `mode`). A
/// shared *vocab* enum (`Arg::Enum`). Plain step (zero-order hold) is no longer a mode — that
/// is the wire's automatic materialize; `m2s` exists only for the gap-filling policies:
/// `Smooth` (one-pole), `Slew` (rate-limited), `Glide` (fixed-time ramp). `Smooth` is the default
/// (the natural knob feel).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, reuben_macros::ArgValue)]
pub enum M2sMode {
    /// One-pole exponential approach (`time`).
    #[default]
    Smooth,
    /// Rate-limited linear approach (`rate` units/s).
    Slew,
    /// Fixed-time linear ramp to the target (`time`); portamento.
    Glide,
}

/// `map`'s response curve across its range (its `curve`). A shared *vocab* enum (`Arg::Enum`):
/// `Linear` (affine) or `Exponential` (geometric, when both output bounds are positive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, reuben_macros::ArgValue)]
pub enum MapCurve {
    /// Affine remap.
    #[default]
    Linear,
    /// Geometric remap (positive output bounds only).
    Exponential,
}

/// Declare the name→type table [`enum_meta_by_type`] dispatches on and the
/// [`PIPEABLE_ENUM_TYPES`] roster that names it, from one list — a test that has to enumerate the
/// pipeable enums reads the roster instead of restating the arms, so the two cannot drift.
macro_rules! pipeable_enums {
    ($($name:literal => $ty:ty),+ $(,)?) => {
        /// Every vocab **enum** type an `interface.inputs` entry may declare, in declaration
        /// order. The roster half of the [`enum_meta_by_type`] table.
        pub const PIPEABLE_ENUM_TYPES: &[&str] = &[$($name),+];

        /// Resolve a vocab **enum** type by its `Arg`-variant name (`"FilterMode"`, `"SnapDir"`, …)
        /// to its [`EnumMeta`](crate::descriptor::EnumMeta) for port `port_name` — the central
        /// name→type table the **interface pipe** loader uses when an `interface.inputs` entry
        /// declares an enum type (`"type": "FilterMode"`). Operators never come through here (their
        /// contracts name the Rust type directly); only the document-declared pipe does, so this is
        /// the one place a *string* names a vocab enum. `None` for an unknown name — the loader
        /// turns that into a pointed load error. Adding a vocab enum that should be pipeable = one
        /// entry in the [`pipeable_enums!`] list.
        pub fn enum_meta_by_type(
            type_name: &str,
            port_name: &'static str,
        ) -> Option<crate::descriptor::EnumMeta> {
            Some(match type_name {
                $($name => <$ty>::enum_meta(port_name),)+
                _ => return None,
            })
        }
    };
}

pipeable_enums! {
    "GateMode" => GateMode,
    "FilterMode" => FilterMode,
    "Waveform" => Waveform,
    "GrainWindow" => GrainWindow,
    "M2sMode" => M2sMode,
    "MapCurve" => MapCurve,
    "SnapDir" => SnapDir,
    "SnapTarget" => SnapTarget,
}
