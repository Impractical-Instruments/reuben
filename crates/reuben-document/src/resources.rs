//! The resource seam: how a logical source name becomes a decoded buffer.
//!
//! The trait lives here rather than in `reuben-core` because every caller is authoring-side — the
//! loader, the edit verbs, the projections, the Coordinator. The render path never resolves
//! anything; it reads an already-decoded
//! [`ResourceStore`](reuben_core::resources::ResourceStore), which is why the store half stayed
//! below. see rules: authoring-library

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;

use reuben_core::resources::SampleBuffer;

/// Why resolving a resource failed. Always surfaced as a
/// [`LoadWarning`](crate::format::LoadWarning) — never fatal. see rules: authoring-library
#[derive(Debug, Clone)]
pub enum ResolveError {
    /// The source could not be opened or read.
    NotFound(String),
    /// The bytes could not be decoded.
    Decode(String),
    /// The source could not be written — a read-only resolver was asked to write, or the
    /// write itself failed (permissions, missing parent, full disk). Distinct from
    /// [`NotFound`](Self::NotFound): the source is addressable, the *store* refused it.
    Write(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotFound(s) => write!(f, "not found: {s}"),
            ResolveError::Decode(s) => write!(f, "decode failed: {s}"),
            ResolveError::Write(s) => write!(f, "write failed: {s}"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolves a logical source (a file path today) to a decoded [`SampleBuffer`]. An eager,
/// non-RT authoring step; `reuben-native` provides the WAV/filesystem implementation.
/// see rules: authoring-library
pub trait ResourceResolver {
    /// Decode `source` (e.g. a path from the instrument's `resources` table) to a buffer.
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError>;

    /// The canonical identity of `source` — the key the loader uses for the cycle guard and the
    /// per-load dedup caches (two spellings of one source must be one identity, and
    /// that judgment belongs to the resolver seam, not the loader). `referrer` is the canonical
    /// id of the document the reference appears in (`None` for the top-level document), so a
    /// nested patch's own references resolve relative to *its* location, not the root's.
    ///
    /// The loader canonicalizes every source before calling [`resolve`](Self::resolve) /
    /// [`resolve_text`](Self::resolve_text), so implementations receive their own canonical
    /// form back. Defaults to identity (sources are exact keys — right for in-memory and test
    /// resolvers); the filesystem resolver normalizes paths here.
    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        let _ = referrer;
        source.to_string()
    }

    /// Read `source` as **text** — the seam for the instrument-kind resource: a voice
    /// patch path resolves to its JSON, which the core then builds into a sub-`Graph`
    /// ([`load_instrument`](crate::format::load_instrument) recursively, so nested `sample`
    /// resources resolve too). Defaults to [`ResolveError::NotFound`] so a sample-only resolver need
    /// not implement it; the filesystem resolver overrides it to read the file.
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }

    /// Write `text` **back** to `source` — the symmetric half of [`resolve_text`](Self::resolve_text):
    /// the door that resolved a source can also persist to it. `source` is opaque and
    /// door-resolved (a filesystem path natively, a store key on web). see rules: agent-mcp
    ///
    /// The loader canonicalizes `source` before calling [`resolve_text`](Self::resolve_text),
    /// and a write receives that **same** canonical form, so two spellings of one source stay
    /// one identity. Defaults to [`ResolveError::Write`] so a read-only resolver stays
    /// read-only; the filesystem resolver and in-memory resolver override it.
    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        let _ = text;
        Err(ResolveError::Write(format!("read-only resolver: {source}")))
    }
}

/// An in-memory [`ResourceResolver`]: sources are exact map keys, nothing touches a
/// filesystem. The non-file side of the library seam made concrete — an embedded
/// or WASM host registers its patches and decoded samples programmatically and loads
/// instruments with no IO; tests get a self-contained resolver without temp files.
///
/// Identity is the literal key ([`ResourceResolver::canonical`] stays the identity default),
/// so a nested patch's references name library keys, not relative paths.
///
/// `texts` sits behind a [`RefCell`] so [`write_text`](ResourceResolver::write_text) can
/// persist through the trait's `&self` — the same way `FsResolver` writes a file without
/// exclusive access. `!Sync`, which is fine: the resolver is used single-threaded within a
/// load, and the coordinator only ever needs it `Send` (a `RefCell` of `Send` data is `Send`).
#[derive(Debug, Clone, Default)]
pub struct MemoryResolver {
    texts: RefCell<BTreeMap<String, String>>,
    samples: BTreeMap<String, SampleBuffer>,
}

impl MemoryResolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register instrument JSON (or any text resource) under `key`.
    pub fn insert_text(&mut self, key: impl Into<String>, text: impl Into<String>) -> &mut Self {
        self.texts.get_mut().insert(key.into(), text.into());
        self
    }

    /// Register a decoded sample under `key`.
    pub fn insert_sample(&mut self, key: impl Into<String>, buffer: SampleBuffer) -> &mut Self {
        self.samples.insert(key.into(), buffer);
        self
    }
}

impl ResourceResolver for MemoryResolver {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.samples
            .get(source)
            .cloned()
            .ok_or_else(|| ResolveError::NotFound(source.to_string()))
    }

    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        self.texts
            .borrow()
            .get(source)
            .cloned()
            .ok_or_else(|| ResolveError::NotFound(source.to_string()))
    }

    /// Persist `text` under the (canonical) key — the non-file door writing its map. Symmetric
    /// with [`resolve_text`](Self::resolve_text): what you write is what a later resolve reads.
    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.texts
            .borrow_mut()
            .insert(source.to_string(), text.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_resolver_write_text_round_trips_through_shared_ref() {
        let r = MemoryResolver::new();
        // Write through `&self` — the trait's write half, no `&mut` needed.
        let resolver: &dyn ResourceResolver = &r;
        resolver.write_text("patch.json", "{\"v\":3}").unwrap();
        assert_eq!(resolver.resolve_text("patch.json").unwrap(), "{\"v\":3}");
        // A second write to the same source overwrites — one source, one identity.
        resolver.write_text("patch.json", "{\"v\":4}").unwrap();
        assert_eq!(resolver.resolve_text("patch.json").unwrap(), "{\"v\":4}");
    }

    #[test]
    fn read_only_resolver_refuses_writes() {
        // A resolver that overrides nothing keeps the default read-only write.
        struct ReadOnly;
        impl ResourceResolver for ReadOnly {
            fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
                Err(ResolveError::NotFound(source.to_string()))
            }
        }
        let err = ReadOnly.write_text("anything", "x").unwrap_err();
        assert!(matches!(err, ResolveError::Write(_)), "got {err:?}");
    }
}
