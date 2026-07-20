//! Integration: resource resolution identity + the non-file library seam (nesting P7, #122).
//!
//! Two spellings of one source must be **one identity** to the cycle guard and
//! the per-load dedup caches, and that judgment belongs to the resolver seam
//! ([`ResourceResolver::canonical`]), not the loader. These tests drive the loader through
//! resolvers with a non-identity canonical form and through the in-memory library resolver.

use std::cell::Cell;

use reuben_core::resources::{MemoryResolver, ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::{load_instrument, LoadError, Registry};

/// A single-oscillator sub-instrument exposing `freq` in / `audio` out.
const TONE: &str = r#"{
    "instrument": "tone",
    "interface": {
        "inputs":  { "freq": "/osc.freq" },
        "outputs": { "audio": "/osc.audio" }
    },
    "nodes": [ { "type": "oscillator", "address": "/osc" } ],
    "outputs": [ { "node": "/osc", "port": "audio" } ]
}"#;

/// Canonicalizes away a leading `./` (a toy of the filesystem resolver's normalization) and
/// counts fetches, so a test can assert two spellings share one fetch + one identity.
struct StripDotSlash {
    text: &'static str,
    fetches: Cell<usize>,
}

impl StripDotSlash {
    fn new(text: &'static str) -> Self {
        Self {
            text,
            fetches: Cell::new(0),
        }
    }
}

impl ResourceResolver for StripDotSlash {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, _source: &str) -> Result<String, ResolveError> {
        self.fetches.set(self.fetches.get() + 1);
        Ok(self.text.to_string())
    }
    fn canonical(&self, source: &str, _referrer: Option<&str>) -> String {
        source.strip_prefix("./").unwrap_or(source).to_string()
    }
}

#[test]
fn two_spellings_of_one_source_share_one_fetch_and_identity() {
    // Diamond reuse through two spellings: both ids canonicalize to `tone.json`, so the child
    // is fetched + parsed once and both nodes still splice (diamond reuse is legal, not a cycle).
    const NESTED: &str = r#"{
        "instrument": "nested",
        "resources": { "t1": "tone.json", "t2": "./tone.json" },
        "nodes": [
            { "type": "subpatch", "address": "/a", "patch": "t1", "inputs": { "freq": 220.0 } },
            { "type": "subpatch", "address": "/b", "patch": "t2", "inputs": { "freq": 330.0 } }
        ],
        "outputs": [ { "node": "/a", "port": "audio" }, { "node": "/b", "port": "audio" } ]
    }"#;

    let resolver = StripDotSlash::new(TONE);
    let loaded = load_instrument(NESTED, &Registry::builtin(), &resolver).expect("load");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert_eq!(
        resolver.fetches.get(),
        1,
        "two spellings of one source must fetch once"
    );
    // Both reuses spliced: each contributes its own oscillator node.
    assert!(loaded.graph.find("/a/osc").is_some());
    assert!(loaded.graph.find("/b/osc").is_some());
}

#[test]
fn cycle_across_spellings_is_caught_on_canonical_identity() {
    // The patch references itself as `./self.json` while loaded as `self.json`: canonical
    // identity catches the cycle at the first re-entry (a known caveat, resolved).
    const SELF_REF: &str = r#"{
        "instrument": "self",
        "resources": { "me": "./self.json" },
        "nodes": [ { "type": "subpatch", "address": "/inner", "patch": "me" } ]
    }"#;
    const HOST: &str = r#"{
        "instrument": "host",
        "resources": { "s": "self.json" },
        "nodes": [ { "type": "subpatch", "address": "/s", "patch": "s" } ]
    }"#;

    match load_instrument(HOST, &Registry::builtin(), &StripDotSlash::new(SELF_REF)) {
        Err(LoadError::CyclicResource { source }) => assert_eq!(source, "self.json"),
        Err(other) => panic!("expected CyclicResource, got {other:?}"),
        Ok(_) => panic!("self-cycle must be fatal"),
    }
}

#[test]
fn memory_resolver_serves_nested_patches_and_samples_without_io() {
    // The non-file library seam made concrete: instrument + sample both come from memory.
    // The nested child names its sample by library key; keys are exact (identity canonical).
    const CHILD: &str = r#"{
        "instrument": "hit",
        "interface": { "outputs": { "audio": "/sample.audio" } },
        "resources": { "blip": "lib/blip" },
        "nodes": [ { "type": "sample", "address": "/sample", "sample": "blip" } ],
        "outputs": [ { "node": "/sample", "port": "audio" } ]
    }"#;
    const PARENT: &str = r#"{
        "instrument": "parent",
        "resources": { "hit": "lib/hit" },
        "nodes": [ { "type": "subpatch", "address": "/h", "patch": "hit" } ],
        "outputs": [ { "node": "/h", "port": "audio" } ]
    }"#;

    let mut lib = MemoryResolver::new();
    lib.insert_text("lib/hit", CHILD);
    lib.insert_sample(
        "lib/blip",
        SampleBuffer::new(vec![vec![0.5, -0.5]], 48_000.0),
    );

    let loaded = load_instrument(PARENT, &Registry::builtin(), &lib).expect("load");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert!(loaded.graph.find("/h/sample").is_some());
}
