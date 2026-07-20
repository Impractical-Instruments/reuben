//! Ready-made instrument graphs.
//!
//! The default rig is defined as **data** (`instruments/default.json`), loaded through the core
//! registry — not hand-built in Rust. Notes arrive as OSC at `/voicer/notes [midi, gate]`. The
//! rig's `voicer` hosts a voice sub-patch (`voices/default-voice.json`); both files are
//! embedded and resolved in-memory so the binary stays self-contained. Load a different instrument
//! file to swap the whole rig.

use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::{load_instrument, Graph, Registry};

/// The default instrument, embedded so the binary is self-contained.
pub const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");

/// The default rig's voice sub-patch, embedded alongside [`DEFAULT_JSON`].
const DEFAULT_VOICE_JSON: &str = include_str!("../../../instruments/voices/default-voice.json");

/// Resolves the embedded default rig's instrument-resources in-memory, so the default rig
/// loads with no filesystem access. Only the default voice patch is known; anything else is absent.
struct EmbeddedVoices;

impl ResourceResolver for EmbeddedVoices {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        match source {
            "voices/default-voice.json" => Ok(DEFAULT_VOICE_JSON.to_string()),
            other => Err(ResolveError::NotFound(other.to_string())),
        }
    }
}

/// The default polyphonic playable rig, built from [`DEFAULT_JSON`] with its voice sub-patch
/// resolved from the embedded copy.
pub fn default_rig() -> Graph {
    load_instrument(DEFAULT_JSON, &Registry::builtin(), &EmbeddedVoices)
        .expect("default.json is a valid instrument")
        .graph
}
