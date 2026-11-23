//! The resource seam: the one call *in*.
//!
//! Nothing is handed to the engine as bytes — a document names its samples and its nested children
//! by opaque `source`, and the host says what a source resolves to. The trait is the contract; the
//! implementation is the host's. [`FsResolver`](crate::FsResolver) is one, behind a default-off
//! feature.
//!
//! Compiled with the `authoring` feature and only with it: a source to resolve is a document's.
//! Both of that half's callers reach it — a document verb reads and writes through it, and so does
//! the load behind [`engine::install_initial`](crate::engine::install_initial).

use alloc::format;
use alloc::string::{String, ToString};

use crate::surface::SurfaceKind;

// The engine seam the adapters below present a store through.
use reuben_document::resources::ResourceResolver;

/// Decoded audio, planar per channel at the file's native rate.
pub use reuben_core::resources::SampleBuffer;

/// Why a host could not produce a resource: not there, not decodable, not writable. Never fatal to
/// a load — a document whose sample is missing loads and reports the failure as a warning.
pub use reuben_document::resources::ResolveError;

/// What [`Resources::create_text`] did: wrote the text, or found the source taken.
pub use reuben_document::resources::Created;

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

    /// Whether `source` is there to decode, without decoding it: `Ok` when it is. A read that
    /// reports port metadata asks this instead of [`read_samples`](Self::read_samples).
    ///
    /// Defaults to a decode, which is right for a store that cannot tell otherwise; a store that
    /// can stat overrides it, and a store that wraps another forwards it.
    fn check_samples(&self, source: &str) -> Result<(), ResolveError> {
        self.read_samples(source).map(|_| ())
    }

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

    /// Write `text` to `source` only if nothing is there, and say which happened — what a verb
    /// that makes a new document writes through, so it can never replace something.
    ///
    /// The default asks [`read_text`](Self::read_text) and then writes: it sees only what reads
    /// as text, and leaves a gap between the two steps. A store that can test for anything at
    /// all at the source, in one step with the write, should override it.
    fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
        if self.read_text(source).is_ok() {
            return Ok(Created::Taken);
        }
        self.write_text(source, text).map(|()| Created::Written)
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

    /// Where `instrument`'s surface doc of `surface` kind lives, or where the first write to it
    /// creates it. `instrument` is a canonical source and so is the answer, so a
    /// [`read_text`](Self::read_text) or [`write_text`](Self::write_text) on it names the same
    /// document. `None` for a store that keeps no surface docs, and for an instrument it cannot
    /// place.
    fn surface_source(&self, instrument: &str, surface: SurfaceKind) -> Option<String> {
        let _ = (instrument, surface);
        None
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

/// `store` with the top document's references resolved from `referrer`, as the Coordinator's
/// `prepare_document_from` resolves them: what scopes a store to one document.
#[cfg(any(feature = "authoring", test))]
pub(crate) struct Anchored<'a> {
    pub store: &'a dyn Resources,
    pub referrer: String,
}

#[cfg(any(feature = "authoring", test))]
impl Resources for Anchored<'_> {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.store.read_samples(source)
    }

    fn check_samples(&self, source: &str) -> Result<(), ResolveError> {
        self.store.check_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        self.store.read_text(source)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.store
            .canonical(source, referrer.or(Some(&self.referrer)))
    }

    fn surface_source(&self, instrument: &str, surface: SurfaceKind) -> Option<String> {
        self.store.surface_source(instrument, surface)
    }
}

/// `store` scoped to one document, as a filesystem door scopes a store with
/// `FsResolver::for_document`: a reference with no referrer, which is how a load resolves the top
/// document's own references, resolves beside `origin`, while each of `documents`, the names the
/// caller spelled for the documents it acts on, still answers to itself. Writes pass through.
#[cfg(any(feature = "authoring", test))]
pub(crate) struct ScopedTo<'a> {
    pub store: &'a dyn Resources,
    pub documents: &'a [String],
    pub origin: &'a str,
}

#[cfg(any(feature = "authoring", test))]
impl Resources for ScopedTo<'_> {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.store.read_samples(source)
    }

    fn check_samples(&self, source: &str) -> Result<(), ResolveError> {
        self.store.check_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        self.store.read_text(source)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.store.write_text(source, text)
    }

    fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
        self.store.create_text(source, text)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        match referrer {
            Some(referrer) => self.store.canonical(source, Some(referrer)),
            None if self.documents.iter().any(|d| d == source) => {
                self.store.canonical(source, None)
            }
            None => self.store.canonical(source, Some(self.origin)),
        }
    }

    fn surface_source(&self, instrument: &str, surface: SurfaceKind) -> Option<String> {
        self.store.surface_source(instrument, surface)
    }
}

/// `store` with every sample read only checked, never decoded: a present sample reads as an empty
/// buffer. For the reads that report port metadata and never touch audio.
#[cfg(any(feature = "authoring", test))]
pub(crate) struct Undecoded<'a>(pub &'a dyn Resources);

#[cfg(any(feature = "authoring", test))]
impl Resources for Undecoded<'_> {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.0.check_samples(source).map(|()| SampleBuffer::empty())
    }

    fn check_samples(&self, source: &str) -> Result<(), ResolveError> {
        self.0.check_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        self.0.read_text(source)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.0.write_text(source, text)
    }

    fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
        self.0.create_text(source, text)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.0.canonical(source, referrer)
    }

    fn surface_source(&self, instrument: &str, surface: SurfaceKind) -> Option<String> {
        self.0.surface_source(instrument, surface)
    }
}

/// [`Adapter`] for a store the engine must **own** rather than borrow: the Coordinator holds its
/// resolver for the session (an open resolves through it), so the borrowed form cannot
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

    fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
        self.0.create_text(source, text)
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

    fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
        self.0.create_text(source, text)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.0.canonical(source, referrer)
    }
}

impl<R: Resources + ?Sized> Resources for alloc::sync::Arc<R> {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        (**self).read_samples(source)
    }

    fn check_samples(&self, source: &str) -> Result<(), ResolveError> {
        (**self).check_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        (**self).read_text(source)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        (**self).write_text(source, text)
    }

    fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
        (**self).create_text(source, text)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        (**self).canonical(source, referrer)
    }

    fn surface_source(&self, instrument: &str, surface: SurfaceKind) -> Option<String> {
        (**self).surface_source(instrument, surface)
    }
}

impl<R: Resources + ?Sized> Resources for alloc::boxed::Box<R> {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        (**self).read_samples(source)
    }

    fn check_samples(&self, source: &str) -> Result<(), ResolveError> {
        (**self).check_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        (**self).read_text(source)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        (**self).write_text(source, text)
    }

    fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
        (**self).create_text(source, text)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        (**self).canonical(source, referrer)
    }

    fn surface_source(&self, instrument: &str, surface: SurfaceKind) -> Option<String> {
        (**self).surface_source(instrument, surface)
    }
}
