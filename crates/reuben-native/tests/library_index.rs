//! Tier-1 staleness gate for the generated library index (ADR-0057 §4, patch-pipeline R4):
//! the committed `instruments/index.md` must equal a fresh generation over the checkout's
//! available-set — the same regenerate-and-compare pattern as the schema's
//! `committed_schema_is_in_sync`. Registry- or instrument-varying content is generated or
//! CI-keyed, never hand-kept (ADR-0051, ADR-0059 §2 plugin posture).

use std::path::PathBuf;

use reuben_core::registry::Registry;
use reuben_native::library::generate_library_index;

/// The committed artifact — compile-time bound so a deleted file fails as loudly as a stale one.
const COMMITTED_INDEX: &str = include_str!("../../../instruments/index.md");

/// Absolute path to the workspace `instruments/` directory, independent of test CWD.
fn instruments_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../instruments")
}

#[test]
fn library_index_is_in_sync() {
    let fresh =
        generate_library_index(&instruments_dir(), &Registry::builtin()).expect("generate index");
    assert_eq!(
        fresh, COMMITTED_INDEX,
        "instruments/index.md is stale — run `cargo run -p reuben-native --example gen_library_index`"
    );
}

#[test]
fn generation_is_deterministic() {
    // The index is a build artifact consumed byte-for-byte (bundled web prefix, MCP resource):
    // two generations over the same checkout must be identical bytes.
    let reg = Registry::builtin();
    let a = generate_library_index(&instruments_dir(), &reg).expect("first generation");
    let b = generate_library_index(&instruments_dir(), &reg).expect("second generation");
    assert_eq!(a, b, "generation must be byte-deterministic");
}

#[test]
fn every_available_instrument_has_a_line() {
    // No curated list (ADR-0057 §4): the index covers the whole available-set — one line per
    // document under instruments/, keyed by the document's own `instrument` name, wherever the
    // file lives (roles are never read off a path, ADR-0057 §2).
    let dir = instruments_dir();
    let index = generate_library_index(&dir, &Registry::builtin()).expect("generate index");

    let mut documents = Vec::new();
    let mut pending = vec![dir];
    while let Some(d) = pending.pop() {
        for entry in std::fs::read_dir(&d).expect("read instruments dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "json") {
                documents.push(path);
            }
        }
    }
    assert!(
        !documents.is_empty(),
        "the available-set sweep found no instrument documents"
    );

    for path in &documents {
        let json = std::fs::read_to_string(path).expect("read document");
        let doc: serde_json::Value = serde_json::from_str(&json).expect("parse document");
        let name = doc["instrument"].as_str().expect("document names itself");
        assert!(
            index.lines().any(|l| l.starts_with(&format!("{name} —"))),
            "{} ({name}) has no index line",
            path.display()
        );
    }
}
