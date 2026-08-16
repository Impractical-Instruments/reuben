//! Ready-made instrument graphs.
//!
//! The default rig is `instruments/default.json`, embedded so the binary is self-contained. Notes
//! arrive as OSC at `/voicer/notes [midi, gate]`. Its `voicer` hosts a voice sub-patch
//! (`voices/default-voice.json`), which resolves through the session's own store like any other
//! nested reference — the rig is embedded, its children are not.

/// The default instrument, embedded so the binary is self-contained.
pub const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");
