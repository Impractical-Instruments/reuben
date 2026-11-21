//! The open instrument's tree: the top-level document plus every document it reaches through a
//! `voice` or `patch` resource, each as the server reads it.

use std::path::Path;

use reuben_api::authoring::{
    InstrumentStructure, InstrumentTree, ReadInstrumentTree, ResourceEntry, StructureNode,
};
use reuben_api::engine::{call_tool, Channel};
use reuben_api::tools::names;

/// The node slot whose document runs as a voice: a Plan of its own, hosted by the node.
const VOICE_SLOT: &str = "voice";

/// The node slots whose resource is a document rather than a sample.
const DOCUMENT_SLOTS: [&str; 2] = [VOICE_SLOT, "patch"];

/// The resolved source of the document `node` of `structure` references as its voice or its
/// patch: the source a load of `structure` reads it from.
pub fn nested<'a>(structure: &'a InstrumentStructure, node: &StructureNode) -> Option<&'a str> {
    reference(structure, node).map(|entry| entry.canonical.as_str())
}

/// The `resources` entry through which `node` of `structure` references its voice or its patch.
fn reference<'a>(
    structure: &'a InstrumentStructure,
    node: &StructureNode,
) -> Option<&'a ResourceEntry> {
    let reference = (node.resources.iter())
        .find(|reference| DOCUMENT_SLOTS.contains(&reference.slot.as_str()))?;
    (structure.resources.iter()).find(|entry| entry.id == reference.id)
}

/// One document of the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// The server's name for this document, which names it to every verb: the name the instrument
    /// was opened by for the top-level document, the referencing resource's resolved source for
    /// every other.
    pub source: String,
    /// The document as the server read it, or why it could not.
    pub read: Result<InstrumentStructure, String>,
}

impl Document {
    /// The resolved source of the document the node at `address` references, when this document
    /// reads and has such a node.
    pub fn nested(&self, address: &str) -> Option<&str> {
        self.reference(address)
            .map(|entry| entry.canonical.as_str())
    }

    /// The `resources` entry the node at `address` references its voice or its patch through,
    /// when this document reads and has such a node.
    pub fn reference(&self, address: &str) -> Option<&ResourceEntry> {
        let structure = self.read.as_ref().ok()?;
        let node = structure.nodes.iter().find(|n| n.address == address)?;
        reference(structure, node)
    }

    /// Whether the node at `address` runs the document it references as a voice, a Plan of its
    /// own, rather than splicing it in as a patch.
    pub fn hosts(&self, address: &str) -> bool {
        let Ok(structure) = &self.read else {
            return false;
        };
        (structure.nodes.iter())
            .find(|node| node.address == address)
            .is_some_and(|node| node.resources.iter().any(|r| r.slot == VOICE_SLOT))
    }

    /// The document's `instrument` name, or its file stem when it does not read.
    pub fn name(&self) -> String {
        match &self.read {
            Ok(structure) => structure.instrument.clone(),
            Err(_) => (Path::new(&self.source).file_stem())
                .map_or_else(|| self.source.clone(), |s| s.to_string_lossy().into_owned()),
        }
    }

    /// The content hash of what was read, when it read.
    pub fn hash(&self) -> Option<&str> {
        self.read
            .as_ref()
            .ok()
            .map(|structure| structure.hash.as_str())
    }

    /// Why this document holds the instrument back from playing as it is: it does not read, or
    /// its load reports errors. Empty when it does not.
    pub fn faults(&self) -> Vec<String> {
        let name = &self.source;
        match &self.read {
            Err(e) => vec![format!("{name}: {e}")],
            Ok(structure) => (structure.report.errors.iter())
                .map(|error| match &error.node {
                    Some(node) => format!("{name}: {node}: {}", error.message),
                    None => format!("{name}: {}", error.message),
                })
                .collect(),
        }
    }
}

/// Every document of the open instrument, the top-level document first and each once.
#[derive(Debug, Clone, PartialEq)]
pub struct Tree {
    documents: Vec<Document>,
}

impl Tree {
    /// The tree of the instrument the server names `top`, in one `read_instrument_tree` call.
    /// `Err` is a call that could not be made or was refused.
    pub fn read(top: &str, channel: &Channel) -> Result<Tree, String> {
        let answer = call_tool(
            names::READ_INSTRUMENT_TREE,
            &ReadInstrumentTree {
                source: top.to_string(),
            },
            channel,
        )
        .map_err(|refusal| refusal.message)?;
        let tree: InstrumentTree = serde_json::from_value(answer.output)
            .map_err(|e| format!("`read_instrument_tree` answered something else: {e}"))?;
        let documents: Vec<Document> = (tree.documents.into_iter())
            .map(|document| Document {
                read: match (document.structure, document.error) {
                    (Some(structure), _) => Ok(structure),
                    (None, error) => Err(error.unwrap_or_else(|| "it was not read".to_string())),
                },
                source: document.source,
            })
            .collect();
        if documents.is_empty() {
            return Err(format!("the server read no document for {top}"));
        }
        Ok(Tree { documents })
    }

    /// A tree of `top` alone, not read, `why` saying so: what a session shows until the server
    /// first answers.
    pub fn unread(top: &str, why: String) -> Tree {
        Tree {
            documents: vec![Document {
                source: top.to_string(),
                read: Err(why),
            }],
        }
    }

    pub fn documents(&self) -> &[Document] {
        &self.documents
    }

    pub fn top(&self) -> &Document {
        &self.documents[0]
    }

    pub fn document(&self, source: &str) -> Option<&Document> {
        self.documents.iter().find(|d| d.source == source)
    }

    /// Why this tree does not play as it is: each fault of each of its documents. Empty when it
    /// does.
    pub fn faults(&self) -> Vec<String> {
        self.documents.iter().flat_map(Document::faults).collect()
    }
}

#[cfg(test)]
mod tests {
    use reuben_native::test_support::within;

    use super::*;
    use crate::session::test_support::Rig;

    const HANG_SECS: u64 = 60;

    fn files(tree: &Tree) -> Vec<&str> {
        tree.documents().iter().map(|d| d.source.as_str()).collect()
    }

    #[test]
    fn the_tree_is_the_top_level_document_and_every_document_it_references() {
        within(HANG_SECS, || {
            let rig = Rig::open("tree_references", "default.json");
            let tree = Tree::read(&rig.top_source(), &rig.channel()).expect("read");
            let sources = files(&tree);
            assert_eq!(sources.len(), 2, "{sources:?}");
            assert_eq!(sources[0], rig.top_source());
            assert!(Path::new(sources[1]).ends_with("voices/default-voice.json"));
            assert_eq!(tree.faults(), Vec::<String>::new());
            assert!(tree.documents().iter().all(|d| d.hash().is_some()));
        });
    }

    /// `acid-techno` nests documents that reference documents of their own, from a directory that
    /// is not the top-level file's.
    #[test]
    fn a_nested_documents_own_references_are_in_the_tree_once() {
        within(HANG_SECS, || {
            let rig = Rig::open("tree_nested", "acid-techno.json");
            let tree = Tree::read(&rig.top_source(), &rig.channel()).expect("read");
            assert_eq!(tree.faults(), Vec::<String>::new());
            let sources = files(&tree);
            assert!(
                (sources.iter()).any(|s| Path::new(s).ends_with("voices/shaped-vca.json")),
                "{sources:?}"
            );
            let mut once = sources.clone();
            once.sort();
            once.dedup();
            assert_eq!(once.len(), sources.len(), "each document once");
        });
    }

    #[test]
    fn a_nested_document_that_does_not_parse_is_a_fault_of_its_own() {
        within(HANG_SECS, || {
            let rig = Rig::open("tree_broken", "default.json");
            std::fs::write(
                rig.lib.join("voices/default-voice.json"),
                "{ not an instrument",
            )
            .unwrap();
            let tree = Tree::read(&rig.top_source(), &rig.channel()).expect("read");
            assert_eq!(tree.documents().len(), 2);
            assert!(tree.documents()[1].read.is_err());
            assert!(
                (tree.faults().iter()).any(|f| f.starts_with(&tree.documents()[1].source)),
                "{:?}",
                tree.faults()
            );
        });
    }
}
