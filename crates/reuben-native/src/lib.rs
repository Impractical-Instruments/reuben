//! reuben-native — the removable native layer (ADR-0012).
//!
//! This crate is the seam where OS-specific I/O lives: audio device output (cpal),
//! OSC UDP adapters, MIDI/Link adapters. It is intentionally a stub for the
//! "first sound" run — the portable [`reuben_core`] does all the work, and proving
//! the seam exists from line one is the point. Audio and OSC land in run 2.

/// Re-export so embedders only depend on this crate.
pub use reuben_core;
