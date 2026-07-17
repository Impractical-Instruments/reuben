//! Regenerate the committed intent-vocabulary rendered view from its curated source.
//!
//! Run after editing `docs/agents/vocabulary.json`:
//! `cargo run -p reuben-core --example gen_vocabulary`
//! The `committed_rendered_view_is_in_sync` test fails if the committed file is stale, and the
//! registry sweep runs here too, so a stale row fails at regeneration time rather than in CI.

use std::path::Path;

use reuben_core::registry::Registry;
use reuben_core::vocabulary::Vocabulary;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/agents");
    let source =
        std::fs::read_to_string(dir.join("vocabulary.json")).expect("read vocabulary.json");
    let vocabulary = Vocabulary::parse(&source).unwrap_or_else(|e| panic!("{e}"));
    let stale = vocabulary.sweep(&Registry::builtin());
    assert!(
        stale.is_empty(),
        "vocabulary rows reference departed registry entries:\n  {}",
        stale.join("\n  ")
    );
    let out = dir.join("vocabulary.md");
    std::fs::write(&out, vocabulary.render()).expect("write vocabulary.md");
    println!("wrote {}", out.display());
}
