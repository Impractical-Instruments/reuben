//! The prior documents a restore returns to, and the verb that returns to them.
//!
//! No consumer of the window holds document text: a caller names an earlier state of a document by
//! the content hash an edit reported for it, and the text behind that hash stays here.
//! [`DocumentHistory`] wraps a host's [`Resources`] and is itself one, so every verb takes it
//! unchanged; [`restore_instrument`] takes nothing else.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};

use std::sync::{Mutex, MutexGuard, PoisonError};

use reuben_core::Registry;
use reuben_document::edit as core_edit;

use super::args::RestoreInstrument;
use super::result::EditResult;
use super::verbs::{hash_document, run_edit, Answer, Refusal};
use crate::resources::{Adapter, ResolveError, Resources, SampleBuffer};

/// A [`Resources`] that remembers the documents written through it, so
/// [`restore_instrument`] can put one back.
///
/// Each write records two texts — what the source held before it and what was written — each under
/// its content hash *and* the canonical source it was at. A hash seen only at another source is not
/// held for this one: a restore returns a file to a state that file was in.
///
/// In memory, and bounded by a document count: recording past the bound evicts the entry recorded
/// longest ago, across all sources. Recording a text already held counts as recording it again, so
/// a state that keeps being returned to is not the next to go.
///
/// Reads and sample resolution go straight to the wrapped store.
pub struct DocumentHistory<R> {
    // A lock rather than a `RefCell` so wrapping a `Sync` store leaves it `Sync`: a host that
    // shares its store across threads must be able to opt in without restructuring. Never
    // contended on the render thread — nothing in the authoring half runs there.
    held: Mutex<VecDeque<Held>>,
    capacity: usize,
    inner: R,
}

struct Held {
    source: String,
    hash: String,
    text: String,
}

impl<R: Resources> DocumentHistory<R> {
    /// The document count [`new`](Self::new) retains.
    pub const DEFAULT_CAPACITY: usize = 256;

    /// Wrap `inner`, retaining [`DEFAULT_CAPACITY`](Self::DEFAULT_CAPACITY) documents.
    pub fn new(inner: R) -> Self {
        Self::with_capacity(inner, Self::DEFAULT_CAPACITY)
    }

    /// Wrap `inner`, retaining at most `capacity` documents across every source. One write can
    /// record two, so a bound under 2 cannot hold both sides of an edit.
    pub fn with_capacity(inner: R, capacity: usize) -> Self {
        DocumentHistory {
            held: Mutex::new(VecDeque::new()),
            capacity,
            inner,
        }
    }

    /// The wrapped store.
    pub fn inner(&self) -> &R {
        &self.inner
    }

    /// Hand back the wrapped store, dropping every held document.
    pub fn into_inner(self) -> R {
        self.inner
    }

    fn held(&self) -> MutexGuard<'_, VecDeque<Held>> {
        // Every critical section leaves the queue whole, so a panic elsewhere while it was held
        // poisons nothing worth refusing over.
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The content hash of `text`, or `None` for text that is not a document — which no hash names
    /// and no restore can ask for.
    fn hash(&self, text: &str) -> Option<String> {
        hash_document(text, &Registry::builtin(), &Adapter(&self.inner)).ok()
    }

    fn record(&self, source: &str, hash: String, text: String) {
        if self.capacity == 0 {
            return;
        }
        let mut held = self.held();
        held.retain(|h| !(h.source == source && h.hash == hash));
        while held.len() >= self.capacity {
            held.pop_front();
        }
        held.push_back(Held {
            source: source.to_string(),
            hash,
            text,
        });
    }

    /// Crate-private on purpose: handing this text to a consumer is the thing the store exists to
    /// make unnecessary.
    fn text_at(&self, source: &str, hash: &str) -> Option<String> {
        let source = self.inner.canonical(source, None);
        self.held()
            .iter()
            .find(|h| h.source == source && h.hash == hash)
            .map(|h| h.text.clone())
    }
}

impl<R: Resources> Resources for DocumentHistory<R> {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.inner.read_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        self.inner.read_text(source)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        // Hashed before the write: normalizing resolves through the store, and the hash a caller
        // was given for this text was minted against the store as it stood then.
        let before = self
            .inner
            .read_text(source)
            .ok()
            .and_then(|text| Some((self.hash(&text)?, text)));
        self.inner.write_text(source, text)?;
        let source = self.inner.canonical(source, None);
        if let Some((hash, before)) = before {
            self.record(&source, hash, before);
        }
        if let Some(hash) = self.hash(text) {
            self.record(&source, hash, text.to_string());
        }
        Ok(())
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.inner.canonical(source, referrer)
    }
}

/// Put `source` back to the state whose content hash is `to`.
///
/// The same pipeline as every document verb — re-normalize, validate whole, write only if valid —
/// so a prior document that no longer validates (a file it references has changed since) answers
/// `written: false` with its report; there is no forcing it. `expect` is mandatory, and a miss is
/// the ordinary answer every guarded verb gives. The write is recorded like any other, so the state
/// restored *from* can be restored to.
///
/// Refuses a `to` that `resources` does not hold for this source — never written through it there,
/// or evicted — and a source whose current text is not a document, which has no hash for `expect`
/// to match.
///
/// Not a roster verb.
pub fn restore_instrument<R: Resources>(
    args: &RestoreInstrument,
    resources: &DocumentHistory<R>,
) -> Result<Answer<EditResult>, Refusal> {
    let prior = resources.text_at(&args.source, &args.to).ok_or_else(|| {
        Refusal::new(format!(
            "no document with content hash {} is held for `{}`: it was never written there \
             through this store, or it has been evicted",
            args.to, args.source
        ))
    })?;
    run_edit(
        &args.source,
        &Some(args.expect.clone()),
        resources,
        |src, reg, res| core_edit::restore_instrument(src, &prior, reg, res),
    )
}

#[cfg(test)]
mod tests;
