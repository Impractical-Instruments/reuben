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
//! variant to [`Arg`](crate::message::Arg). The OSC flat-multi-arg boundary conversion for
//! struct types lands in phase 6.
//!
//! Types live next to their behavior — [`Harmony`] and its resolver in
//! [`crate::harmony`], [`Pitch`]/[`Note`] in [`crate::pitch`] — and are re-exported here so a
//! consumer reaches the whole vocabulary through one path (`crate::vocab::*`).

pub use crate::harmony::{Chord, ChordTag, Harmony, ScaleField, SnapDir, SnapPolicy, SnapTarget};
pub use crate::pitch::{Note, Pitch};

/// How a sequencer step drives its output (the sequencer's `gate_mode`). A shared *vocab* enum
/// (`Arg::GateMode`): emit a pitched **degree** per step, or a bare **gate** trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, reuben_macros::ArgValue)]
pub enum GateMode {
    /// Emit a degree (pitched) per active step.
    #[default]
    Degree,
    /// Emit a bare gate/trigger per active step.
    Gate,
}
