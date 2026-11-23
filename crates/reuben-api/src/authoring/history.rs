//! The prior documents a restore returns to, and the verb that returns to them.
//!
//! No consumer of the window holds document text: a caller names an earlier state of a document by
//! the content hash an edit reported for it, and the text behind that hash stays here.
//! [`DocumentHistory`] wraps a host's [`Resources`] and is itself one, so every verb takes it
//! unchanged; [`restore_instrument`] takes nothing else.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use std::sync::{Mutex, MutexGuard, PoisonError};

use reuben_core::Registry;
use reuben_document::edit as core_edit;
use reuben_document::{content_hash, NormalizedDoc};

use super::args::RestoreInstrument;
use super::result::EditResult;
use super::result::Report;
use super::verbs::{edit_summary, expect_conflict, expect_miss, Answer, Refusal};
use crate::resources::{Adapter, Created, ResolveError, Resources, SampleBuffer};
use crate::surface::{render_surface, surface_hash, SurfaceDoc, SurfaceKind};

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

    /// Whether [`restore_instrument`] can find the state `hash` for `source` — the one refusal a
    /// caller holding a stack of hashes must tell apart from a failed read or write, since only
    /// this one means the hash is gone for good.
    pub fn holds(&self, source: &str, hash: &str) -> bool {
        let source = self.inner.canonical(source, None);
        self.held()
            .iter()
            .any(|h| h.source == source && h.hash == hash)
    }

    fn held(&self) -> MutexGuard<'_, VecDeque<Held>> {
        // Every critical section leaves the queue whole, so a panic elsewhere while it was held
        // poisons nothing worth refusing over.
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The content hash of `text` and the canonical text that hash is of, or `None` for text that
    /// is not a document — which no hash names and no restore can ask for.
    ///
    /// The canonical text is what gets held, not `text`: an older-format document takes part of
    /// its meaning from the files it references when it is normalized, so the same raw text read
    /// again later can normalize to a different document than the one this hash names.
    ///
    /// A surface doc has no Instrument to normalize against, so text that is not an Instrument is
    /// tried as one.
    fn mint(&self, text: &str) -> Option<(String, String)> {
        if let Ok(doc) =
            NormalizedDoc::from_json(text, &Registry::builtin(), Some(&Adapter(&self.inner)))
        {
            return Some((content_hash(&doc), doc.to_json_pretty()));
        }
        let doc = SurfaceDoc::parse(text).ok()?;
        Some((surface_hash(&doc), doc.to_json_pretty()))
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

    /// Private on purpose: handing this text to a consumer is the thing the store exists to
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

    fn check_samples(&self, source: &str) -> Result<(), ResolveError> {
        self.inner.check_samples(source)
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
            .and_then(|text| self.mint(&text));
        self.inner.write_text(source, text)?;
        let source = self.inner.canonical(source, None);
        if let Some((hash, before)) = before {
            self.record(&source, hash, before);
        }
        if let Some((hash, written)) = self.mint(text) {
            self.record(&source, hash, written);
        }
        Ok(())
    }

    // Handed on rather than left to the default, which reads and then writes: the wrapped store's
    // own create is what refuses a file that appears between the two.
    fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
        let created = self.inner.create_text(source, text)?;
        if created == Created::Written {
            if let Some((hash, written)) = self.mint(text) {
                self.record(&self.inner.canonical(source, None), hash, written);
            }
        }
        Ok(created)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.inner.canonical(source, referrer)
    }

    fn surface_source(&self, instrument: &str, surface: SurfaceKind) -> Option<String> {
        self.inner.surface_source(instrument, surface)
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
    restore_instrument_through(args, resources, resources)
}

/// [`restore_instrument`] with the prior text taken from `history` and everything else — the guard,
/// the validation, the write — done through `resources`, a store scoped to the document as an edit
/// to it is.
pub(crate) fn restore_instrument_through<R: Resources>(
    args: &RestoreInstrument,
    history: &DocumentHistory<R>,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    let prior = history.text_at(&args.source, &args.to).ok_or_else(|| {
        Refusal::new(format!(
            "no document with content hash {} is held for `{}`: it was never written there \
             through this store, or it has been evicted",
            args.to, args.source
        ))
    })?;
    if let Ok(held) = SurfaceDoc::parse(&prior) {
        return restore_surface(args, &prior, &held, resources);
    }
    let registry = Registry::builtin();
    let resolver = Adapter(resources);
    let expect = Some(args.expect.clone());
    if let Some(conflict) = expect_conflict(&expect, &args.source, &registry, &resolver) {
        return Ok(conflict);
    }
    let restored = core_edit::restore_instrument(
        &args.source,
        &prior,
        &args.to,
        &args.expect,
        &registry,
        &resolver,
    )
    .map_err(|why| Refusal::new(why.to_string()))?;
    match restored {
        Some(result) => {
            let output = EditResult::from_core(result);
            let summary = edit_summary(&output);
            Ok(Answer { output, summary })
        }
        // The source changed after the guard's own read: answer as the guard would have.
        None => expect_conflict(&expect, &args.source, &registry, &resolver).ok_or_else(|| {
            Refusal::new(format!(
                "`{}` changed while it was being restored; read it again and retry",
                args.source
            ))
        }),
    }
}

/// The restore of a surface doc: no Instrument validation, because a surface doc is allowed to
/// name controls its Instrument lacks, and refusing the way back to one would strand an undo.
fn restore_surface(
    args: &RestoreInstrument,
    prior: &str,
    held: &SurfaceDoc,
    resources: &dyn Resources,
) -> Result<Answer<EditResult>, Refusal> {
    let kind = if args.source.ends_with(".midi.json") {
        SurfaceKind::Midi
    } else {
        SurfaceKind::Screen
    };
    let current = resources
        .read_text(&args.source)
        .map_err(|e| Refusal::new(format!("could not read `{}`: {e}", args.source)))
        .and_then(|text| {
            SurfaceDoc::parse(&text).map_err(|e| {
                Refusal::new(format!(
                    "`{}` is not a surface doc, so there is no hash for `expect` to match: {e}",
                    args.source
                ))
            })
        })?;
    let actual = surface_hash(&current);
    if actual != args.expect {
        let zoom = render_surface(&current, kind);
        return Ok(expect_miss(actual, &args.expect, zoom));
    }
    resources
        .write_text(&args.source, &format!("{prior}\n"))
        .map_err(|e| Refusal::new(format!("could not write `{}`: {e}", args.source)))?;
    let output = EditResult {
        report: Report {
            ok: true,
            errors: Vec::new(),
            warnings: Vec::new(),
        },
        pre_existing: Vec::new(),
        written: true,
        hash: args.to.clone(),
        notes: Vec::new(),
        zoom: render_surface(held, kind),
    };
    let summary = edit_summary(&output);
    Ok(Answer { output, summary })
}

#[cfg(test)]
mod tests;
