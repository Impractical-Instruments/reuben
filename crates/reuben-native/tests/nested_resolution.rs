//! Integration: filesystem resolution for nested instruments (nesting P7, #122).
//!
//! Two properties the unit tests can't show end-to-end:
//! - **Per-document base**: a nested patch's own references resolve relative to *its*
//!   directory, transitively — a library patch can bundle private sub-patches next to itself.
//! - **Library-root fallback**: a reference that doesn't exist next to its referencing
//!   document comes from the configured instrument root instead (sibling-first).

use std::path::Path;

use reuben_core::{load_instrument, Registry};
use reuben_native::resources::FsResolver;

const LEAF: &str = r#"{
    "instrument": "leaf",
    "interface": {
        "inputs":  { "freq": "/osc.freq" },
        "outputs": { "audio": "/osc.audio" }
    },
    "nodes": [ { "type": "oscillator", "address": "/osc" } ]
}"#;

/// A mid-level patch that references its **sibling** `leaf.json` — only resolvable if the
/// loader rebases the child's references onto the child's own directory.
const MID: &str = r#"{
    "instrument": "mid",
    "interface": { "outputs": { "audio": "/inner.audio" } },
    "resources": { "leaf": "leaf.json" },
    "nodes": [ { "type": "subpatch", "address": "/inner", "patch": "leaf" } ]
}"#;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn nested_patch_references_resolve_relative_to_the_nested_file() {
    let dir = std::env::temp_dir().join("reuben_nested_res/proj");
    write(&dir.join("sub/mid.json"), MID);
    write(&dir.join("sub/leaf.json"), LEAF);
    // A decoy at the top level proves the child's ref is NOT resolved against the root
    // document's directory: resolving `leaf.json` there would find invalid JSON and die.
    write(&dir.join("leaf.json"), "not json");

    const TOP: &str = r#"{
        "instrument": "top",
        "resources": { "mid": "sub/mid.json" },
        "nodes": [ { "type": "subpatch", "address": "/m", "patch": "mid" } ],
        "outputs": [ { "node": "/m", "port": "audio" } ]
    }"#;

    let resolver = FsResolver::new(&dir);
    let loaded = load_instrument(TOP, &Registry::builtin(), &resolver).expect("load");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert!(
        loaded.graph.find("/m/inner/osc").is_some(),
        "leaf spliced through two nesting levels"
    );

    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("reuben_nested_res"));
}

#[test]
fn missing_sibling_reference_comes_from_the_instrument_root() {
    let base = std::env::temp_dir().join("reuben_root_fallback/proj");
    let root = std::env::temp_dir().join("reuben_root_fallback/lib");
    write(&root.join("tone.json"), LEAF);
    std::fs::create_dir_all(&base).unwrap();

    const TOP: &str = r#"{
        "instrument": "top",
        "resources": { "tone": "tone.json" },
        "nodes": [ { "type": "subpatch", "address": "/t", "patch": "tone" } ],
        "outputs": [ { "node": "/t", "port": "audio" } ]
    }"#;

    // Without the root: unresolved — the nest dissolves dark with a warning.
    let bare = FsResolver::new(&base);
    let loaded = load_instrument(TOP, &Registry::builtin(), &bare).expect("load");
    assert!(!loaded.warnings.is_empty(), "no root: must warn unresolved");
    assert!(loaded.graph.find("/t/osc").is_none());

    // With the root: the library copy resolves and splices.
    let rooted = FsResolver::new(&base).with_root(&root);
    let loaded = load_instrument(TOP, &Registry::builtin(), &rooted).expect("load");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert!(loaded.graph.find("/t/osc").is_some());

    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("reuben_root_fallback"));
}
