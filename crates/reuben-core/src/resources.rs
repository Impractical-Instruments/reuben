//! Resources — decoded audio as a shared, bank-ready read service (ADR-0016).
//!
//! The sample player is the first operator that depends on **external bytes** (an audio
//! file) which must be resolved and decoded before render. Three existing contracts make
//! that awkward — zero-arg type-erased construction, `f32`-only params, and an
//! allocation-free RT `process` — so decoded audio does not live on the operator's
//! construction path. Instead it lives in a central [`ResourceStore`] built by the
//! Coordinator at load time (single-writer, ADR-0012) and read **immutable** by Render.
//!
//! The accessors here are written as **pure functions of `(id, channel, frame)`**: the
//! resident v1.1 implementation indexes a decoded buffer, and the future streaming "audio
//! bank" consults a warm-block cache behind the *same* signatures, so the operator never
//! re-plumbs. Determinism (ADR-0001) is preserved because a read always returns the same
//! float for the same arguments; a bank that falls behind underruns (an xrun) rather than
//! substituting silence.
//!
//! Codecs and filesystem IO stay out of this portable crate (ADR-0007, ADR-0012): the
//! [`ResourceResolver`] trait is the seam `reuben-native` fills with a WAV decoder.

use std::collections::BTreeMap;
use std::fmt;

/// A handle to a decoded resource within a [`ResourceStore`]. `Copy`, cheap to carry on an
/// operator and through [`Operator::spawn`](crate::operator::Operator::spawn).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SampleId(usize);

/// Decoded audio: **every** channel, stored planar, at the file's native sample rate
/// (ADR-0016). Channels are kept (not downmixed) so a player can pick or mix them and so
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
    /// to so the player outputs silence (ADR-0016).
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
/// (ADR-0012, ADR-0016). Resident-only in v1.1 — every resource is decoded up front and
/// held forever; the accessors' signatures are what the future streaming bank reuses.
#[derive(Debug, Default)]
pub struct ResourceStore {
    buffers: Vec<SampleBuffer>,
    by_id: BTreeMap<String, SampleId>,
}

impl ResourceStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a decoded buffer under a logical id, returning its handle. The loader
    /// dedups, so each unique id is inserted exactly once.
    pub fn insert(&mut self, id: impl Into<String>, buffer: SampleBuffer) -> SampleId {
        let sid = SampleId(self.buffers.len());
        self.buffers.push(buffer);
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
    /// function** — the bank-ready read seam (ADR-0016): the resident impl indexes the
    /// buffer; the future bank consults a warm-block cache behind this same signature, so
    /// the player is unchanged when streaming lands.
    pub fn sample(&self, id: SampleId, channel: usize, frame: usize) -> f32 {
        self.buf(id).sample(channel, frame)
    }
}

/// The resolved resource handles for one node, keyed by descriptor resource-slot name
/// (ADR-0016). The loader fills it and hands it to
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
/// [`LoadWarning`](crate::format::LoadWarning) — never fatal (ADR-0016): a missing or bad
/// sample binds to an empty buffer and the node plays silence, so one broken file never
/// takes down a live rig.
#[derive(Debug, Clone)]
pub enum ResolveError {
    /// The source could not be opened or read.
    NotFound(String),
    /// The bytes could not be decoded.
    Decode(String),
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ResolveError::NotFound(s) => write!(f, "not found: {s}"),
            ResolveError::Decode(s) => write!(f, "decode failed: {s}"),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Resolves a logical source (a file path today) to a decoded [`SampleBuffer`] (ADR-0016).
///
/// The seam that keeps codecs and filesystem IO out of the portable core (ADR-0007,
/// ADR-0012, the boundary-adapter pattern): `reuben-native` provides the WAV/filesystem
/// implementation, and compressed formats or non-file sources (a bundle, a network — the
/// "library" thread) drop in behind the same trait without touching core. Resolution is an
/// eager, non-RT authoring step.
pub trait ResourceResolver {
    /// Decode `source` (e.g. a path from the instrument's `resources` table) to a buffer.
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError>;

    /// Read `source` as **text** — the seam for the instrument-kind resource (ADR-0032 §2): a voice
    /// patch path resolves to its JSON, which the core then builds into a sub-`Graph`
    /// ([`load_instrument`](crate::format::load_instrument) recursively, so nested `sample`
    /// resources resolve too). Defaults to [`ResolveError::NotFound`] so a sample-only resolver need
    /// not implement it; the filesystem resolver overrides it to read the file.
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
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
}
