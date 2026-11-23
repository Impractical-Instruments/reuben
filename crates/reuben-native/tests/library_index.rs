//! Staleness gate for the generated library index: the committed `instruments/index.md` must
//! equal a fresh generation over the checkout's available-set — the same regenerate-and-compare
//! pattern as the vocabulary's `committed_rendered_view_is_in_sync`. Registry- or
//! instrument-varying content is generated or CI-keyed, never hand-kept.

use std::path::PathBuf;

use reuben_native::library::generate_library_index;

/// The committed artifact — compile-time bound so a deleted file fails as loudly as a stale one.
const COMMITTED_INDEX: &str = include_str!("../../../instruments/index.md");

/// Absolute path to the workspace `instruments/` directory, independent of test CWD.
fn instruments_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../instruments")
}

#[test]
fn library_index_is_in_sync() {
    let fresh = generate_library_index(&instruments_dir()).expect("generate index");
    assert_eq!(
        fresh, COMMITTED_INDEX,
        "instruments/index.md is stale — run `cargo run -p reuben-native --example gen_library_index`"
    );
}

#[test]
fn generation_is_deterministic() {
    // The index is a build artifact consumed byte-for-byte (bundled web prefix, MCP resource):
    // two generations over the same checkout must be identical bytes.
    let a = generate_library_index(&instruments_dir()).expect("first generation");
    let b = generate_library_index(&instruments_dir()).expect("second generation");
    assert_eq!(a, b, "generation must be byte-deterministic");
}

#[test]
fn every_available_instrument_has_a_line() {
    // No curated list: the index covers the whole available-set — one line per
    // document under instruments/, keyed by the document's own `instrument` name, wherever the
    // file lives (roles are never read off a path).
    let dir = instruments_dir();
    let index = generate_library_index(&dir).expect("generate index");

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

/// A reusable child that passes a `str` through its boundary indexes like any other: its `str`
/// output is inert at the root (a warning), not an error that would fail the whole generation.
#[test]
fn a_child_with_a_str_output_indexes() {
    let dir = std::env::temp_dir().join(format!("reuben_index_str_child_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(
        dir.join("str_child.json"),
        r#"{ "format_version": 3, "instrument": "str_child",
             "doc": "A reusable child that passes a pattern string through.",
             "interface": { "inputs": { "pat": { "type": "str", "default": "inner" } },
                            "outputs": { "o": { "from": "/pat.out" } } },
             "nodes": [] }"#,
    )
    .expect("write fixture");
    let index = generate_library_index(&dir);
    std::fs::remove_dir_all(&dir).ok();
    let index = index.expect("generates");
    assert!(index.contains("str_child"), "{index}");
}
