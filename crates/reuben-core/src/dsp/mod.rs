//! Raw DSP building blocks — pure per-sample math, shared across operators.
//!
//! Everything here is graph-agnostic: no ports, no `Io`, no descriptors — just the
//! arithmetic. Operators own control semantics (which inputs exist, how they're read,
//! when coefficients are recomputed) and embed these components for the sample math.
//!
//! Components are **value-oriented**: state is a small `Copy` struct a `process` loop
//! copies to a local, ticks in registers, and writes back once per block (see [`svf`]).

pub mod svf;
