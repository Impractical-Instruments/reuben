//! vocab — the shared concrete types that ride the central [`Arg`](crate::message::Arg)
//! (ADR-0030).
//!
//! These are the **domain vocabulary**: defined once and reused across operators, which is
//! what lets [`Arg`](crate::message::Arg) stay a *closed* enum while still carrying rich types
//! (a `SnapTarget` duplicated per-operator was the smell ADR-0030 removes). Each type carries
//! `#[derive(ArgValue)]` (`crate::ArgValue`), which generates its `Arg` integration —
//! `From`/`TryFrom` — plus, for enums, the Enum-over-OSC table (`VARIANTS` / `from_symbol` /
//! `resolve_arg` / `enum_meta`).
//!
//! Adding a domain type = define it here (or beside its logic), derive `ArgValue`, and add one
//! variant to [`Arg`](crate::message::Arg). A struct type that should cross the OSC boundary
//! also hand-implements [`OscArg`](crate::message::OscArg) (its flat multi-arg form,
//! `from_osc`/`to_osc`) and self-registers the converter beside that impl with
//! `crate::register_osc_form!` (epic #146) — [`Note`] does; [`Harmony`] deliberately does
//! neither (the boundary opt-out; its wire form is issue #209).
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

/// The state-variable filter's output tap (the filter's `mode`, ADR-0022). A shared *vocab* enum
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

/// How `m2s` fills the dense per-sample gaps between sparse messages (its `mode`, ADR-0017). A
/// shared *vocab* enum (`Arg::Enum`). Plain step (zero-order hold) is no longer a mode — that
/// is the wire's automatic materialize (ADR-0030); `m2s` exists only for the gap-filling policies:
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

/// Resolve a vocab **enum** type by its `Arg`-variant name (`"FilterMode"`, `"SnapDir"`, …) to
/// its [`EnumMeta`](crate::descriptor::EnumMeta) for port `port_name` — the central name→type
/// table the **interface pipe** loader (ADR-0038) uses when an `interface.inputs` entry declares
/// an enum type (`"type": "FilterMode"`). Operators never come through here (their contracts
/// name the Rust type directly); only the document-declared pipe does, so this match is the one
/// place a *string* names a vocab enum. `None` for an unknown name — the loader turns that into
/// a pointed load error. Adding a vocab enum that should be pipeable = one arm here.
pub fn enum_meta_by_type(
    type_name: &str,
    port_name: &'static str,
) -> Option<crate::descriptor::EnumMeta> {
    Some(match type_name {
        "GateMode" => GateMode::enum_meta(port_name),
        "FilterMode" => FilterMode::enum_meta(port_name),
        "Waveform" => Waveform::enum_meta(port_name),
        "GrainWindow" => GrainWindow::enum_meta(port_name),
        "M2sMode" => M2sMode::enum_meta(port_name),
        "MapCurve" => MapCurve::enum_meta(port_name),
        "SnapDir" => SnapDir::enum_meta(port_name),
        "SnapTarget" => SnapTarget::enum_meta(port_name),
        _ => return None,
    })
}
