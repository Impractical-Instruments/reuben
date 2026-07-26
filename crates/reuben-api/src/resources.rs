//! The resource seam: the one call *in*.
//!
//! Nothing is handed to the engine as bytes — a document names its samples and its nested children
//! by opaque `source`, and the host says what a source resolves to. The trait is the contract; the
//! implementation is the host's. [`FsResolver`](crate::FsResolver) is one, behind a default-off
//! feature.
//!
//! It sits above both halves rather than inside [`authoring`](crate::authoring) because both call
//! it: a document verb reads and writes through it, and so does the load that builds the initial
//! Engine a host renders ([`render::install_initial`](crate::render::install_initial)). One seam,
//! whichever half a host compiles.
//!
//! see rules: authoring-library

// The engine seam the adapters below present a store through; with neither half compiled there is
// nothing to present it to.
#[cfg(any(feature = "authoring", feature = "render", test))]
use reuben_core::resources::ResourceResolver;

/// Decoded audio, planar per channel at the file's native rate.
pub use reuben_core::resources::SampleBuffer;

/// Why a host could not produce a resource: not there, not decodable, not writable. Never fatal to
/// a load — a document whose sample is missing loads and reports the failure as a warning.
pub use reuben_core::resources::ResolveError;

/// The host's answer to "what does this `source` name?".
///
/// `source` is opaque to the window: a filesystem path for the native and sidecar doors, a store
/// key in a browser. A store that only serves samples implements [`read_samples`](Self::read_samples)
/// and takes the defaults; a store that also holds documents implements the text half, and only a
/// store that can persist overrides [`write_text`](Self::write_text) — a document verb asked to
/// write through a read-only store fails rather than silently dropping the edit.
pub trait Resources {
    /// Decode `source` to audio.
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError>;

    /// Read `source` as text: an instrument document, its own or a nested child's.
    ///
    /// Defaults to [`NotFound`](ResolveError::NotFound) so a sample-only store need not
    /// implement it.
    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }

    /// Persist `text` back to `source` — the symmetric half of [`read_text`](Self::read_text), and
    /// the only way a document verb's edit becomes durable. Defaults to refusing, so a read-only
    /// store stays read-only.
    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        let _ = text;
        Err(ResolveError::Write(format!("read-only store: {source}")))
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

/// Presents a window-owned [`Resources`] as the engine's own resolver seam.
///
/// The window declares the *trait*, because that is what a host implements against and it must be
/// nameable without naming the engine. It does not declare the two types crossing it: a decoded
/// buffer and a resolve failure are things a host hands **in**, not shapes the window serializes
/// **out**, so a twin would buy nothing and cost a conversion per resource. The wire types — the
/// ones a model reads — are the window's own; these are re-exports.
///
/// Borrowed, because every verb that takes one hands it straight down to a call that returns before
/// the verb does. The one caller that cannot borrow takes [`OwnedAdapter`].
#[cfg(any(feature = "authoring", test))]
pub(crate) struct Adapter<'a>(pub &'a dyn Resources);

/// [`Adapter`] for a store the engine must **own** rather than borrow: the Coordinator holds its
/// resolver for the session (a by-path swap resolves through it), so the borrowed form cannot
/// serve it.
///
/// `Send` is the Coordinator's requirement, not this seam's: the handle it lives in crosses to
/// whatever thread the host swaps on.
#[cfg(feature = "render")]
pub(crate) struct OwnedAdapter<R>(pub R);

#[cfg(feature = "render")]
impl<R: Resources> ResourceResolver for OwnedAdapter<R> {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.0.read_samples(source)
    }

    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        self.0.read_text(source)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.0.write_text(source, text)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.0.canonical(source, referrer)
    }
}

#[cfg(any(feature = "authoring", test))]
impl ResourceResolver for Adapter<'_> {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.0.read_samples(source)
    }

    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        self.0.read_text(source)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.0.write_text(source, text)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.0.canonical(source, referrer)
    }
}
