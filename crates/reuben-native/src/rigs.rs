//! Ready-made instrument graphs.
//!
//! The default rig is now defined as **data** (`instruments/default.json`), loaded through
//! the core registry — not hand-built in Rust. This is the same Voicer -> Oscillator ->
//! Filter -> Envelope -> Output chain that produced the "first sound"; notes arrive as OSC
//! at `/voicer/note [midi, gate]`. Load a different instrument file to swap the whole rig.

use reuben_core::{load, Graph, Registry};

/// The default instrument, embedded so the binary is self-contained.
pub const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");

/// The default monophonic playable rig, built from [`DEFAULT_JSON`].
pub fn default_rig() -> Graph {
    load(DEFAULT_JSON, &Registry::builtin()).expect("default.json is a valid instrument")
}
