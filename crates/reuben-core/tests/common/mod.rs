//! Shared integration-test resolver: text resources from a repo directory, mirroring the
//! production `FsResolver` discipline in one place. (The benches keep their own richer copy —
//! wav decoding + `..` normalization — in `benches/common`.)

use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};

/// Text resources from a repo directory. Keys are relative to the root the resolver is built
/// with — `Dir("instruments")` mirrors loading a top-level instrument,
/// `Dir("instruments/voices")` mirrors `reuben play` on a voice document (the
/// `FsResolver::for_instrument` base). Sample resolution errors: the corpora these tests load
/// are sample-free.
pub struct Dir(pub &'static str);

impl ResourceResolver for Dir {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }

    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/../../{}/{source}", env!("CARGO_MANIFEST_DIR"), self.0);
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }

    /// Per-document rebase (the `FsResolver` discipline, ADR-0034 §1): a nested document's own
    /// references (kick-voice.json's `shaped-vca.json`) resolve next to *it*, keys staying
    /// root-relative.
    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        match referrer.and_then(|r| r.rsplit_once('/')) {
            Some((dir, _)) => format!("{dir}/{source}"),
            None => source.to_string(),
        }
    }
}
