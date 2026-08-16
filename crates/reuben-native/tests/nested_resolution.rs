//! Integration: filesystem resolution for nested instruments.
//!
//! Two properties the unit tests can't show end-to-end:
//! - **Per-document base**: a nested patch's own references resolve relative to *its*
//!   directory, transitively — a library patch can bundle private sub-patches next to itself.
//! - **Library-root fallback**: a reference that doesn't exist next to its referencing
//!   document comes from the configured instrument root instead (sibling-first).
//!
//! Both are read the way a host reads them: install the document through the window and render it.
//! A leaf that spliced is a leaf that makes sound; one that dissolved is silence with a warning.

use std::path::Path;

use reuben_api::engine::{install_initial, LoadWarning};
use reuben_api::render::{AudioConfig, RenderSide, RenderSlot};
use reuben_api::FsResolver;

const LEAF: &str = r#"{
    "instrument": "leaf",
    "interface": {
        "inputs":  { "freq": "/osc.freq" },
        "outputs": { "audio": "/osc.audio" }
    },
    "nodes": [ { "type": "oscillator", "address": "/osc" } ],
    "outputs": [ { "node": "/osc", "port": "audio" } ]
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

/// The block [`peak`] sizes its buffer for, so the two cannot drift apart.
const BLOCK: usize = 256;

/// Install `doc` through the window. The Coordinator is dropped: these cases never swap, and the
/// render side alone is what carries the resolved graph.
fn install(doc: &str, store: FsResolver) -> (RenderSide, Vec<LoadWarning>) {
    let (_coordinator, side, warnings) =
        install_initial(doc, store, AudioConfig::new(48_000.0, BLOCK)).expect("install");
    (side, warnings)
}

/// Render a block and report the peak: a spliced oscillator hums, a dissolved nest is silent.
fn peak(side: RenderSide) -> f32 {
    let mut slot = RenderSlot::new(side);
    let mut buf = vec![0.0f32; BLOCK * slot.channels().max(1)];
    slot.fill(&mut buf);
    buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
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

    let (side, warnings) = install(TOP, FsResolver::new(&dir));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(
        peak(side) > 0.0,
        "the leaf spliced through two nesting levels and is sounding"
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
    let (side, warnings) = install(TOP, FsResolver::new(&base));
    assert!(!warnings.is_empty(), "no root: must warn unresolved");
    assert_eq!(peak(side), 0.0, "a dissolved nest sounds nothing");

    // With the root: the library copy resolves and splices.
    let (side, warnings) = install(TOP, FsResolver::new(&base).with_root(&root));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert!(peak(side) > 0.0, "the library copy spliced and is sounding");

    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("reuben_root_fallback"));
}
