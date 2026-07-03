//! Raw DSP building blocks — pure per-sample math, shared across operators.
//!
//! Everything here is graph-agnostic: no ports, no `Io`, no descriptors — just the
//! arithmetic. Operators own control semantics (which inputs exist, how they're read,
//! when coefficients are recomputed) and embed these components for the sample math.
//!
//! Components are **value-oriented** on purpose: state types are small `Copy` structs a
//! `process` loop copies into a local, ticks in registers, and writes back to the operator
//! once per block. Threading state as a value is what lets LLVM keep it out of memory —
//! mutating operator fields through `&mut self` inside the sample loop spills every
//! sample instead (#169).

pub mod svf;
