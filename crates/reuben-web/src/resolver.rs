//! Fetch-on-miss in-memory resource resolution — the web side of the resource seam
//! (ADR-0016, ADR-0034 §1).
//!
//! In the browser the core's resolution model meets two hard constraints at once:
//! resolution is **eager** (every referenced document and sample is resolved at load time,
//! before render), and the load ultimately happens inside an `AudioWorklet`, which cannot
//! `fetch()` — all bytes must already be staged in memory before the graph is constructed.
//! So a plain [`MemoryResolver`](reuben_core::resources::MemoryResolver)-style lookup is
//! necessary but not sufficient: something has to *discover* which keys need staging, and
//! that knowledge lives in the instrument JSON, which only the WASM-side loader parses.
//!
//! [`WebResolver`] closes that loop by turning every miss into a work item instead of only
//! degrading. When the loader asks for a key that is not staged, the resolver records a
//! [`Miss`] — the canonical key plus what [`ResourceKind`] of bytes it wanted — and returns
//! [`ResolveError::NotFound`] so the load still completes (degraded, per ADR-0016's
//! never-fatal rule). The main thread then drains [`WebResolver::take_misses`], fetches
//! `assetBase + key` for each, stages the bytes, and reloads — repeating until a load
//! records zero misses. JS stays **schema-blind** throughout: it never parses instrument
//! JSON to find references; the WASM says what it wants next.
//!
//! Identity is the resolver's job (ADR-0034 §1: two spellings of one source must be one
//! cycle-guard/dedup key). With no filesystem to consult, [`WebResolver`]'s
//! [`canonical`](reuben_core::resources::ResourceResolver::canonical) is purely lexical and
//! **root-relative**: keys look like `samples/blip.wav`, resolved against the referring
//! document's directory, with `.`/`..` folded away and `..` clamped at the root — a key can
//! never escape the asset root, so every canonical key is a safe URL suffix.

use std::cell::RefCell;
use std::collections::BTreeMap;

use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};

/// What kind of resource a miss wants, so the loader (JS) fetches and stages it correctly.
/// The u32 values are ABI: 0 = Text, 1 = Sample (crossing the C-ABI miss list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    /// Instrument/voice/patch JSON — fetch as text, stage via [`WebResolver::stage_text`].
    Text = 0,
    /// Audio — fetch, decode (e.g. `decodeAudioData`), stage via
    /// [`WebResolver::stage_sample`].
    Sample = 1,
}

/// One recorded miss: the canonical root-relative key and what kind of bytes it wants.
///
/// The key is exactly what [`ResourceResolver::resolve`]/[`resolve_text`] received — the
/// loader canonicalizes before calling, so the main thread can fetch `assetBase + key`
/// verbatim and stage the result under the same key.
///
/// [`resolve_text`]: ResourceResolver::resolve_text
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Miss {
    /// Canonical root-relative key, e.g. `samples/blip.wav`.
    pub key: String,
    /// What kind of bytes the load wanted at that key.
    pub kind: ResourceKind,
}

/// An in-memory [`ResourceResolver`] that records what it *couldn't* serve.
///
/// Staged resources are exact-key lookups (like the core's `MemoryResolver`); a lookup that
/// misses records a [`Miss`] and degrades. Miss recording uses a [`RefCell`] because the
/// trait's methods take `&self` — fine here: `wasm32-unknown-unknown` is single-threaded and
/// host-side tests drive the resolver from one thread, so `WebResolver` deliberately does
/// not promise `Sync`.
#[derive(Debug, Default)]
pub struct WebResolver {
    /// Staged instrument/voice/patch JSON, by canonical root-relative key.
    texts: BTreeMap<String, String>,
    /// Staged decoded samples, by canonical root-relative key.
    samples: BTreeMap<String, SampleBuffer>,
    /// Misses recorded since the last [`take_misses`](WebResolver::take_misses), deduped,
    /// in first-miss order. Interior mutability: `resolve`/`resolve_text` take `&self`.
    misses: RefCell<Vec<Miss>>,
}

impl WebResolver {
    /// An empty resolver: everything misses until staged.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage instrument/voice/patch JSON under its canonical root-relative key.
    pub fn stage_text(&mut self, key: impl Into<String>, text: impl Into<String>) {
        self.texts.insert(key.into(), text.into());
    }

    /// Stage a decoded sample under its canonical root-relative key.
    pub fn stage_sample(&mut self, key: impl Into<String>, buffer: SampleBuffer) {
        self.samples.insert(key.into(), buffer);
    }

    /// Drain the misses recorded since the last take (deduped, in first-miss order).
    ///
    /// The fetch-on-miss loop: load, take the misses, fetch + stage each, and reload —
    /// until this returns empty, at which point the load was fully served.
    pub fn take_misses(&self) -> Vec<Miss> {
        self.misses.take()
    }

    /// Drop all staged resources and recorded misses (toy switching re-stages from scratch).
    pub fn clear(&mut self) {
        self.texts.clear();
        self.samples.clear();
        self.misses.get_mut().clear();
    }

    /// Record a miss for `key`, deduped: the same key + kind is recorded once per take.
    /// (The loader may ask repeatedly — e.g. once per hosted voice copy — but the main
    /// thread should fetch each asset once per round.)
    fn record_miss(&self, key: &str, kind: ResourceKind) {
        let mut misses = self.misses.borrow_mut();
        if !misses.iter().any(|m| m.kind == kind && m.key == key) {
            misses.push(Miss {
                key: key.to_string(),
                kind,
            });
        }
    }
}

impl ResourceResolver for WebResolver {
    /// Canonical identity = lexical root-relative normalization (ADR-0034 §1); no
    /// filesystem, so this is pure string work.
    ///
    /// The base is the referrer's directory (the part left of its last `/`; a referrer with
    /// no `/`, or no referrer at all, resolves from the root). A leading `/` in a source is
    /// an empty segment and drops, so an "absolute" spelling resolves referrer-relative
    /// like any other (repo instruments never use one). Base and source are joined
    /// and folded segment by segment: empty and `.` segments drop, `..` pops the segment
    /// stack. Popping past the root just drops the `..` — **deliberately clamped**: a key
    /// like `../../escape.wav` cannot name anything outside the asset root, so every
    /// canonical key stays a safe suffix for `assetBase + key`.
    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        let base = referrer
            .and_then(|r| r.rsplit_once('/'))
            .map_or("", |(dir, _file)| dir);
        let mut stack: Vec<&str> = Vec::new();
        for seg in base.split('/').chain(source.split('/')) {
            match seg {
                "" | "." => {}
                ".." => {
                    // Clamped at root: popping an empty stack drops the `..` (never escapes).
                    stack.pop();
                }
                s => stack.push(s),
            }
        }
        stack.join("/")
    }

    /// Look up a staged sample by exact key (the loader already canonicalized). A miss
    /// records `Miss { key, kind: Sample }` and degrades to `NotFound` — the core binds an
    /// empty buffer and warns (ADR-0016), and the main thread fetches for the next round.
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        match self.samples.get(source) {
            Some(buffer) => Ok(buffer.clone()),
            None => {
                self.record_miss(source, ResourceKind::Sample);
                Err(ResolveError::NotFound(source.to_string()))
            }
        }
    }

    /// Look up staged text (instrument/voice/patch JSON) by exact key. A miss records
    /// `Miss { key, kind: Text }` and degrades to `NotFound` — non-fatal at the core
    /// (an empty sub-graph plus a warning), fetched for the next round.
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        match self.texts.get(source) {
            Some(text) => Ok(text.clone()),
            None => {
                self.record_miss(source, ResourceKind::Text);
                Err(ResolveError::NotFound(source.to_string()))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reuben_core::{load_instrument, Registry};

    /// The repo's sampler instrument and its voice patch — the real nested-reference shape
    /// the fetch-on-miss loop exists for (top doc -> `voices/sampler-voice.json` ->
    /// `../samples/blip.wav`).
    const SAMPLER_JSON: &str = include_str!("../tests/fixtures/sampler.json");
    const VOICE_JSON: &str = include_str!("../tests/fixtures/voices/sampler-voice.json");

    fn miss(key: &str, kind: ResourceKind) -> Miss {
        Miss {
            key: key.to_string(),
            kind,
        }
    }

    #[test]
    fn canonical_is_lexical_root_relative_normalization() {
        let r = WebResolver::new();
        assert_eq!(
            r.canonical("voices/sampler-voice.json", None),
            "voices/sampler-voice.json"
        );
        assert_eq!(
            r.canonical("../samples/blip.wav", Some("voices/sampler-voice.json")),
            "samples/blip.wav"
        );
        assert_eq!(r.canonical("./a.json", None), "a.json");
        assert_eq!(r.canonical("x/../a.json", None), "a.json");
        // `..` past the root is clamped, never escapes the asset root.
        assert_eq!(
            r.canonical("../../escape.wav", Some("voices/v.json")),
            "escape.wav"
        );
        // A referrer with no `/` lives at the root, so it rebases nothing.
        assert_eq!(r.canonical("a.json", Some("top.json")), "a.json");
    }

    #[test]
    fn staged_resources_resolve_without_recording_a_miss() {
        let mut r = WebResolver::new();
        r.stage_text("voice.json", "{}");
        r.stage_sample(
            "kick.wav",
            SampleBuffer::new(vec![vec![0.5, -0.5]], 48_000.0),
        );

        assert_eq!(r.resolve_text("voice.json").unwrap(), "{}");
        let buf = r.resolve("kick.wav").unwrap();
        assert_eq!(buf.frame_count(), 2);
        assert_eq!(buf.sample(0, 0), 0.5);
        assert!(r.take_misses().is_empty(), "hits must not record misses");
    }

    #[test]
    fn a_miss_records_the_key_and_the_kind_asked_for() {
        let r = WebResolver::new();
        assert!(matches!(
            r.resolve("samples/blip.wav"),
            Err(ResolveError::NotFound(_))
        ));
        assert!(matches!(
            r.resolve_text("voices/v.json"),
            Err(ResolveError::NotFound(_))
        ));
        assert_eq!(
            r.take_misses(),
            vec![
                miss("samples/blip.wav", ResourceKind::Sample),
                miss("voices/v.json", ResourceKind::Text),
            ]
        );
    }

    #[test]
    fn misses_dedup_on_record_but_kinds_stay_distinct() {
        let r = WebResolver::new();
        let _ = r.resolve("a.wav");
        let _ = r.resolve("a.wav"); // same key + kind: one entry
        let _ = r.resolve_text("a.wav"); // same key, other kind: its own entry
        assert_eq!(
            r.take_misses(),
            vec![
                miss("a.wav", ResourceKind::Sample),
                miss("a.wav", ResourceKind::Text),
            ]
        );
    }

    #[test]
    fn take_misses_drains() {
        let r = WebResolver::new();
        let _ = r.resolve("a.wav");
        assert_eq!(r.take_misses().len(), 1);
        assert!(r.take_misses().is_empty(), "second take is empty");
        // ...and the dedup window resets: the same miss records again after a take.
        let _ = r.resolve("a.wav");
        assert_eq!(r.take_misses().len(), 1);
    }

    #[test]
    fn clear_empties_staged_resources_and_misses() {
        let mut r = WebResolver::new();
        r.stage_text("v.json", "{}");
        r.stage_sample("k.wav", SampleBuffer::new(vec![vec![1.0]], 44_100.0));
        let _ = r.resolve("missing.wav");

        r.clear();
        assert!(r.take_misses().is_empty(), "clear drops recorded misses");
        assert!(
            r.resolve_text("v.json").is_err() && r.resolve("k.wav").is_err(),
            "clear drops staged resources"
        );
    }

    /// The fetch-on-miss discovery loop in miniature, against the real core loader: start
    /// with nothing staged, and let each round's misses say what to stage next — exactly
    /// what the main thread does with `fetch(assetBase + key)` between rounds.
    #[test]
    fn fetch_on_miss_loop_discovers_and_serves_the_sampler() {
        let registry = Registry::builtin();
        let mut resolver = WebResolver::new();

        // Round 1: empty resolver. The load completes degraded (misses are never fatal,
        // ADR-0016) and reports the top doc's one reference: the voice patch, as Text.
        load_instrument(SAMPLER_JSON, &registry, &resolver).expect("degraded load succeeds");
        assert_eq!(
            resolver.take_misses(),
            vec![miss("voices/sampler-voice.json", ResourceKind::Text)]
        );

        // Round 2: stage what round 1 asked for. The voice now loads, and *its* reference
        // surfaces — `../samples/blip.wav` canonicalized against the voice's directory.
        resolver.stage_text("voices/sampler-voice.json", VOICE_JSON);
        load_instrument(SAMPLER_JSON, &registry, &resolver).expect("degraded load succeeds");
        assert_eq!(
            resolver.take_misses(),
            vec![miss("samples/blip.wav", ResourceKind::Sample)]
        );

        // Round 3: stage the sample. Zero misses, warning-free load — staging is complete.
        resolver.stage_sample(
            "samples/blip.wav",
            SampleBuffer::new(vec![vec![0.0, 0.25, -0.25, 0.0]], 48_000.0),
        );
        let loaded = load_instrument(SAMPLER_JSON, &registry, &resolver).expect("full load");
        assert!(resolver.take_misses().is_empty(), "no misses once staged");
        assert!(
            loaded.warnings.is_empty(),
            "fully staged load must be warning-free, got: {:?}",
            loaded.warnings
        );
    }
}
