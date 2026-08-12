//! Tier-1 mechanical guards for the intent-vocabulary artifact: the committed
//! source parses, every move references the live operator registry (the registry-staleness
//! test — a rename breaks the build, not the agent), and the committed rendered view is in
//! sync with the source (the generated-artifact posture: one source, generated view, CI-checked).

use reuben_core::Registry;
use reuben_document::vocabulary::Vocabulary;

const SOURCE: &str = include_str!("../../../docs/agents/vocabulary.json");
const RENDERED: &str = include_str!("../../../docs/agents/vocabulary.md");

fn committed() -> Vocabulary {
    Vocabulary::parse(SOURCE).unwrap_or_else(|e| panic!("docs/agents/vocabulary.json: {e}"))
}

#[test]
fn committed_vocabulary_references_the_live_registry() {
    let stale = committed().sweep(&Registry::builtin());
    assert!(
        stale.is_empty(),
        "docs/agents/vocabulary.json references departed registry entries:\n  {}\n\
         fix the row (or the operator), then run `cargo run -p reuben-core --example gen_vocabulary`",
        stale.join("\n  ")
    );
}

/// The table the verb applies is the table this file sweeps. Two `include_str!`s of one path is two
/// places a path could be edited, and a drift there would leave CI proving a document the engine
/// does not read.
#[test]
fn the_builtin_table_is_the_committed_source() {
    assert_eq!(Vocabulary::builtin().render(), committed().render());
}

#[test]
fn committed_rendered_view_is_in_sync() {
    assert_eq!(
        RENDERED,
        committed().render(),
        "docs/agents/vocabulary.md is stale — run `cargo run -p reuben-core --example gen_vocabulary`"
    );
}
