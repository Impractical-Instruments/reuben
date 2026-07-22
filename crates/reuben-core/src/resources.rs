//! Resources — decoded audio as a shared, bank-ready read service.
//!
//! The sample player is the first operator that depends on **external bytes** (an audio
//! file) which must be resolved and decoded before render. Three existing contracts make
//! that awkward — zero-arg type-erased construction, `f32`-only params, and an
//! allocation-free RT `process` — so decoded audio does not live on the operator's
//! construction path. Instead it lives in a central [`ResourceStore`] built by the
//! Coordinator at load time (single-writer) and read **immutable** by Render.
//!
//! The accessors here are written as **pure functions of `(id, channel, frame)`**: the
//! resident v1.1 implementation indexes a decoded buffer, and the future streaming "audio
//! bank" consults a warm-block cache behind the *same* signatures, so the operator never
//! re-plumbs. Determinism is preserved because a read always returns the same
//! float for the same arguments; a bank that falls behind underruns (an xrun) rather than
//! substituting silence.
//!
//! Codecs and filesystem IO stay out of this portable crate: the
//! [`ResourceResolver`] trait is the seam `reuben-native` fills with a WAV decoder.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

/// A handle to a decoded resource within a [`ResourceStore`]. `Copy`, cheap to carry on an
/// operator and through [`Operator::spawn`](crate::operator::Operator::spawn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleId(usize);

/// Decoded audio: **every** channel, stored planar, at the file's native sample rate
/// Channels are kept (not downmixed) so a player can pick or mix them and so
/// the data model already suits multichannel render. An [`SampleBuffer::empty`] buffer is
/// the degrade-to-silence sentinel for a missing or failed resource.
#[derive(Debug, Clone, Default)]
pub struct SampleBuffer {
    /// One `Vec<f32>` per channel; all the same length (`frames`).
    channels: Vec<Vec<f32>>,
    /// Common length of every channel (the min, defensively).
    frames: usize,
    /// Native sample rate of the decoded file, in Hz (0.0 for an empty buffer).
    sample_rate: f32,
}

impl SampleBuffer {
    /// A buffer from planar per-channel samples and a native sample rate. The frame count
    /// is the shortest channel (channels are expected equal-length).
    pub fn new(channels: Vec<Vec<f32>>, sample_rate: f32) -> Self {
        let frames = channels.iter().map(|c| c.len()).min().unwrap_or(0);
        Self {
            channels,
            frames,
            sample_rate,
        }
    }

    /// The empty (zero-length, zero-channel) buffer — what a missing/failed resource binds
    /// to so the player outputs silence.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Number of channels.
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    /// Number of frames per channel.
    pub fn frame_count(&self) -> usize {
        self.frames
    }

    /// Native sample rate in Hz.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// One decoded sample at `(channel, frame)`, or `0.0` if either is out of range. The
    /// pure read primitive every higher-level accessor is built on.
    pub fn sample(&self, channel: usize, frame: usize) -> f32 {
        self.channels
            .get(channel)
            .and_then(|c| c.get(frame))
            .copied()
            .unwrap_or(0.0)
    }
}

/// The decoded-resource store: built by the loader/Coordinator, read immutable by Render
/// Resident-only in v1.1 — every resource is decoded up front and
/// held forever; the accessors' signatures are what the future streaming bank reuses.
#[derive(Debug, Default)]
pub struct ResourceStore {
    /// `Arc` so several stores can share one decoded buffer: each subpatch reuse and voice
    /// copy builds its own store, and the loader's per-load cache hands them all the same
    /// allocation instead of a deep copy per reference.
    buffers: Vec<Arc<SampleBuffer>>,
    by_id: BTreeMap<String, SampleId>,
}

impl ResourceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a decoded buffer under a logical id, returning its handle. The loader
    /// dedups, so each unique id is inserted exactly once. Takes anything `Arc`-able so
    /// plain owned buffers (tests, drivers) and the loader's shared cache entries both fit.
    pub fn insert(
        &mut self,
        id: impl Into<String>,
        buffer: impl Into<Arc<SampleBuffer>>,
    ) -> SampleId {
        let sid = SampleId(self.buffers.len());
        self.buffers.push(buffer.into());
        self.by_id.insert(id.into(), sid);
        sid
    }

    /// Resolve a logical id to its handle, if present.
    pub fn id(&self, id: &str) -> Option<SampleId> {
        self.by_id.get(id).copied()
    }

    fn buf(&self, id: SampleId) -> &SampleBuffer {
        &self.buffers[id.0]
    }

    /// Channel count of a resource.
    pub fn channels(&self, id: SampleId) -> usize {
        self.buf(id).channel_count()
    }

    /// Frame count of a resource.
    pub fn frames(&self, id: SampleId) -> usize {
        self.buf(id).frame_count()
    }

    /// Native sample rate of a resource, in Hz.
    pub fn sample_rate(&self, id: SampleId) -> f32 {
        self.buf(id).sample_rate()
    }

    /// One decoded sample at `(id, channel, frame)`; `0.0` out of range. A **pure
    /// function** — the bank-ready read seam: the resident impl indexes the
    /// buffer; the future bank consults a warm-block cache behind this same signature, so
    /// the player is unchanged when streaming lands.
    pub fn sample(&self, id: SampleId, channel: usize, frame: usize) -> f32 {
        self.buf(id).sample(channel, frame)
    }
}

/// The resolved resource handles for one node, keyed by descriptor resource-slot name
/// The loader fills it and hands it to
/// [`Operator::bind_resources`](crate::operator::Operator::bind_resources).
#[derive(Debug, Clone, Default)]
pub struct ResolvedRefs {
    refs: BTreeMap<&'static str, SampleId>,
}

impl ResolvedRefs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a resolved handle to a named slot.
    pub fn set(&mut self, slot: &'static str, id: SampleId) {
        self.refs.insert(slot, id);
    }

    /// The handle bound to `slot`, if any.
    pub fn get(&self, slot: &str) -> Option<SampleId> {
        self.refs.get(slot).copied()
    }
}

/// Why resolving a resource failed. Always surfaced as a
/// [`LoadWarning`](crate::format::LoadWarning) — never fatal: a missing or bad
/// sample binds to an empty buffer and the node plays silence, so one broken file never
/// takes down a live rig.
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

/// Resolves a logical source (a file path today) to a decoded [`SampleBuffer`].
///
/// The seam that keeps codecs and filesystem IO out of the portable core (the
/// boundary-adapter pattern): `reuben-native` provides the WAV/filesystem
/// implementation, and compressed formats or non-file sources (a bundle, a network — the
/// "library" thread) drop in behind the same trait without touching core. Resolution is an
/// eager, non-RT authoring step.
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

    /// Write `text` **back** to `source` — the symmetric half of [`resolve_text`](Self::resolve_text),
    /// so a document is a resource the same way a voice patch is: the door that resolved a
    /// source can also persist to it. `source` is opaque and door-resolved (a filesystem path
    /// natively, a store key on web), which is what keeps the path-addressed document API one
    /// contract behind every door — the `#portable-tool-contracts` invariant (see rules: agent-mcp).
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
    fn empty_buffer_reads_silence() {
        let b = SampleBuffer::empty();
        assert_eq!(b.channel_count(), 0);
        assert_eq!(b.frame_count(), 0);
        assert_eq!(b.sample(0, 0), 0.0);
    }

    #[test]
    fn sample_reads_planar_and_bounds_check() {
        let b = SampleBuffer::new(vec![vec![1.0, 2.0], vec![3.0, 4.0]], 44_100.0);
        assert_eq!(b.channel_count(), 2);
        assert_eq!(b.frame_count(), 2);
        assert_eq!(b.sample_rate(), 44_100.0);
        assert_eq!(b.sample(0, 1), 2.0);
        assert_eq!(b.sample(1, 0), 3.0);
        // Out of range on either axis is silence, not a panic.
        assert_eq!(b.sample(2, 0), 0.0);
        assert_eq!(b.sample(0, 9), 0.0);
    }

    #[test]
    fn store_maps_ids_and_reads_through() {
        let mut s = ResourceStore::new();
        let id = s.insert("kick", SampleBuffer::new(vec![vec![0.5, -0.5]], 48_000.0));
        assert_eq!(s.id("kick"), Some(id));
        assert_eq!(s.id("missing"), None);
        assert_eq!(s.channels(id), 1);
        assert_eq!(s.frames(id), 2);
        assert_eq!(s.sample(id, 0, 0), 0.5);
    }

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
