//! Raw DSP building blocks — pure per-sample math, shared across operators.
//!
//! Everything here is graph-agnostic: no ports, no `Io`, no descriptors — just the
//! arithmetic. Operators own control semantics (which inputs exist, how they're read,
//! when coefficients are recomputed) and embed these components for the sample math.
//!
//! Components are **value-oriented**: state is a small `Copy` struct rather than a `&mut self`
//! object, so a `process` loop can hold it in registers.

pub mod svf;
