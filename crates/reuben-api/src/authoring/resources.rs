//! The resource seam: the one call *in*.
//!
//! Nothing is handed to the engine as bytes — a document names its samples and its nested children
//! by opaque `source`, and the host says what a source resolves to. The trait is the contract; the
//! implementation is the host's. [`FsResolver`](crate::FsResolver) is one, behind a default-off
//! feature.
//!
//! see rules: authoring-library

use std::fmt;

use reuben_core::resources::{ResolveError, ResourceResolver};

/// Decoded audio, planar per channel at the file's native rate.
///
/// Re-exported rather than redeclared: it is a buffer a host fills, not a shape anything
/// serializes, so a window-owned twin would buy nothing and cost a copy per resource.
pub use reuben_core::resources::SampleBuffer;

/// Why a host could not produce a resource. Never fatal to a load — a document whose sample is
/// missing loads and reports the failure as a warning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceError {
    /// The source could not be opened or read.
    NotFound(String),
    /// The bytes could not be decoded.
    Decode(String),
    /// The source could not be written: a read-only store was asked to write, or the write itself
    /// failed. Distinct from [`NotFound`](Self::NotFound) — the source is addressable, the store
    /// refused it.
    Write(String),
}

impl fmt::Display for ResourceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResourceError::NotFound(s) => write!(f, "not found: {s}"),
            ResourceError::Decode(s) => write!(f, "decode failed: {s}"),
            ResourceError::Write(s) => write!(f, "write failed: {s}"),
        }
    }
}

impl std::error::Error for ResourceError {}

/// The host's answer to "what does this `source` name?".
///
/// `source` is opaque to the window: a filesystem path for the native and sidecar doors, a store
/// key in a browser. A store that only serves samples implements [`read_samples`](Self::read_samples)
/// and takes the defaults; a store that also holds documents implements the text half, and only a
/// store that can persist overrides [`write_text`](Self::write_text) — a document verb asked to
/// write through a read-only store fails rather than silently dropping the edit.
pub trait Resources {
    /// Decode `source` to audio.
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResourceError>;

    /// Read `source` as text: an instrument document, its own or a nested child's.
    ///
    /// Defaults to [`NotFound`](ResourceError::NotFound) so a sample-only store need not
    /// implement it.
    fn read_text(&self, source: &str) -> Result<String, ResourceError> {
        Err(ResourceError::NotFound(source.to_string()))
    }

    /// Persist `text` back to `source` — the symmetric half of [`read_text`](Self::read_text), and
    /// the only way a document verb's edit becomes durable. Defaults to refusing, so a read-only
    /// store stays read-only.
    fn write_text(&self, source: &str, text: &str) -> Result<(), ResourceError> {
        let _ = text;
        Err(ResourceError::Write(format!("read-only store: {source}")))
    }

    /// The canonical identity of `source` — the key that decides whether two spellings are one
    /// resource, for the cycle guard and the per-load dedup caches. `referrer` is the canonical id
    /// of the document the reference appears in (`None` at the top level), so a nested child's own
    /// references resolve relative to *it*.
    ///
    /// Defaults to identity, which is right for a store whose sources are exact keys; a
    /// path-shaped store normalizes here.
    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        let _ = referrer;
        source.to_string()
    }
}

impl From<ResolveError> for ResourceError {
    fn from(e: ResolveError) -> Self {
        match e {
            ResolveError::NotFound(s) => ResourceError::NotFound(s),
            ResolveError::Decode(s) => ResourceError::Decode(s),
            ResolveError::Write(s) => ResourceError::Write(s),
        }
    }
}

impl From<ResourceError> for ResolveError {
    fn from(e: ResourceError) -> Self {
        match e {
            ResourceError::NotFound(s) => ResolveError::NotFound(s),
            ResourceError::Decode(s) => ResolveError::Decode(s),
            ResourceError::Write(s) => ResolveError::Write(s),
        }
    }
}

/// Presents a window-owned [`Resources`] as the engine's own resolver seam — the whole of what
/// "the API declares its own resolver trait" costs at runtime, once per resolve on an off-thread
/// path.
pub(crate) struct Adapter<'a>(pub &'a dyn Resources);

impl ResourceResolver for Adapter<'_> {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.0.read_samples(source).map_err(Into::into)
    }

    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        self.0.read_text(source).map_err(Into::into)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.0.write_text(source, text).map_err(Into::into)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.0.canonical(source, referrer)
    }
}
