//! The open instrument's file tree: the top-level document plus every document it reaches through
//! a `voice` or `patch` resource, each read as it is on disk.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use reuben_api::authoring::{read_instrument, InstrumentStructure, ReadInstrument, StructureNode};
use reuben_api::FsResolver;

/// The node slots whose resource is a document rather than a sample.
const DOCUMENT_SLOTS: [&str; 2] = ["voice", "patch"];

/// The store one document is read and edited through: scoped to that document, because a
/// document's references resolve from its store's top level. Samples are checked for presence
/// rather than decoded.
pub fn store(source: &str, root: Option<&Path>) -> FsResolver {
    let store = FsResolver::for_document(source).stat_only();
    match root {
        Some(root) => store.with_root(root),
        None => store,
    }
}

/// The resolved source of the document `node` of `structure` references as its voice or its
/// patch: the source a load of `structure` reads it from.
pub fn nested<'a>(structure: &'a InstrumentStructure, node: &StructureNode) -> Option<&'a str> {
    let reference = (node.resources.iter())
        .find(|reference| DOCUMENT_SLOTS.contains(&reference.slot.as_str()))?;
    (structure.resources.iter())
        .find(|entry| entry.id == reference.id)
        .map(|entry| entry.canonical.as_str())
}

/// One file of the tree.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    /// What names this document to a `reuben-api` verb: the path the instrument was opened by for
    /// the top-level document, the resolved source of the referencing resource for every other.
    pub source: String,
    /// Where the document is on disk, absolute.
    pub path: PathBuf,
    /// The document as it is on disk, or why it could not be read.
    pub read: Result<InstrumentStructure, String>,
}

impl Document {
    fn read(source: String, path: PathBuf, root: Option<&Path>) -> Self {
        let read = read_instrument(
            &ReadInstrument {
                source: source.clone(),
            },
            &store(&source, root),
        )
        .map(|answer| answer.output)
        .map_err(|refusal| refusal.message);
        Self { source, path, read }
    }

    /// The resolved sources of the documents this one's nodes reference, in node order.
    fn children(&self) -> Vec<&str> {
        let Ok(structure) = &self.read else {
            return Vec::new();
        };
        (structure.nodes.iter())
            .filter_map(|node| nested(structure, node))
            .collect()
    }

    /// The resolved source of the document the node at `address` references, when this document
    /// reads and has such a node.
    pub fn nested(&self, address: &str) -> Option<&str> {
        let structure = self.read.as_ref().ok()?;
        let node = structure.nodes.iter().find(|n| n.address == address)?;
        nested(structure, node)
    }

    /// The document's `instrument` name, or its file name when it does not read.
    pub fn name(&self) -> String {
        match &self.read {
            Ok(structure) => structure.instrument.clone(),
            Err(_) => (self.path.file_stem())
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
}

/// Every file of the open instrument, the top-level document first and each file once.
#[derive(Debug, Clone, PartialEq)]
pub struct Tree {
    documents: Vec<Document>,
}

impl Tree {
    /// Read the tree of the instrument at `top` from disk. `root` is the library root a reference
    /// falls back to, as for the engine.
    pub fn resolve(top: &Path, root: Option<&Path>) -> Self {
        let top_path = std::path::absolute(top).unwrap_or_else(|_| top.to_path_buf());
        let mut seen = BTreeSet::from([top_path.clone()]);
        let mut documents = vec![Document::read(top.display().to_string(), top_path, root)];
        let mut next = 0;
        while next < documents.len() {
            let children: Vec<String> = documents[next]
                .children()
                .into_iter()
                .map(str::to_string)
                .collect();
            for source in children {
                let path = PathBuf::from(&source);
                if seen.insert(path.clone()) {
                    documents.push(Document::read(source, path, root));
                }
            }
            next += 1;
        }
        Self { documents }
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

    /// Why this tree must not be swapped in; empty when it may be.
    ///
    /// A nested document that is on disk and does not load is one: the engine's load would report
    /// it as a warning and install the instrument without that voice or subpatch, replacing a Plan
    /// that plays with one that does not. A nested document that is not on disk at all is not one
    /// here, since the instrument may never have had it; the session holds the tree when a
    /// document it had loaded goes missing.
    pub fn blocked(&self) -> Vec<String> {
        let mut why = Vec::new();
        for (index, document) in self.documents.iter().enumerate() {
            let name = document.path.display();
            match &document.read {
                Ok(structure) => {
                    for error in &structure.report.errors {
                        why.push(match &error.node {
                            Some(node) => format!("{name}: {node}: {}", error.message),
                            None => format!("{name}: {}", error.message),
                        });
                    }
                    if !structure.report.ok && structure.report.errors.is_empty() {
                        why.push(format!("{name}: does not load"));
                    }
                }
                // A referencing document comes first in the tree and has usually said this
                // already, on the node that holds the reference.
                Err(e) if why.iter().any(|said| said.contains(e.as_str())) => {}
                Err(e) if index == 0 || document.path.exists() => why.push(format!("{name}: {e}")),
                Err(_) => {}
            }
        }
        why
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::Scratch;

    fn paths(tree: &Tree) -> Vec<&Path> {
        tree.documents().iter().map(|d| d.path.as_path()).collect()
    }

    #[test]
    fn the_tree_is_the_top_level_file_and_every_document_it_references() {
        let scratch = Scratch::new("tree_references");
        let lib = scratch.library();
        let tree = Tree::resolve(&lib.join("default.json"), None);
        assert_eq!(
            paths(&tree),
            [
                lib.join("default.json").as_path(),
                lib.join("voices").join("default-voice.json").as_path()
            ]
        );
        assert_eq!(tree.blocked(), Vec::<String>::new());
        assert!(tree.documents().iter().all(|d| d.hash().is_some()));
    }

    /// `acid-techno` nests documents that reference documents of their own, from a directory that
    /// is not the top-level file's: one store reused down the tree would miss them.
    #[test]
    fn a_nested_documents_own_references_resolve_from_its_directory() {
        let scratch = Scratch::new("tree_nested");
        let lib = scratch.library();
        let tree = Tree::resolve(&lib.join("acid-techno.json"), None);
        assert_eq!(tree.blocked(), Vec::<String>::new());
        let unread: Vec<_> = tree
            .documents()
            .iter()
            .filter(|d| d.read.is_err())
            .map(|d| &d.path)
            .collect();
        assert_eq!(unread, Vec::<&PathBuf>::new());
        let nested = lib.join("voices").join("shaped-vca.json");
        assert!(
            paths(&tree).contains(&nested.as_path()),
            "{:?}",
            paths(&tree)
        );
        let mut once = paths(&tree);
        once.sort();
        once.dedup();
        assert_eq!(once.len(), tree.documents().len(), "each file once");
    }

    #[test]
    fn a_reference_only_the_library_root_holds_is_in_the_tree() {
        let scratch = Scratch::new("tree_root");
        let lib = scratch.library();
        let bare = scratch.path("bare").join("default.json");
        std::fs::create_dir_all(bare.parent().unwrap()).unwrap();
        std::fs::copy(lib.join("default.json"), &bare).unwrap();

        let tree = Tree::resolve(&bare, Some(&lib));
        assert_eq!(
            paths(&tree),
            [
                bare.as_path(),
                lib.join("voices").join("default-voice.json").as_path()
            ]
        );
    }

    #[test]
    fn a_nested_document_that_does_not_parse_blocks_and_a_missing_one_does_not() {
        let scratch = Scratch::new("tree_blocked");
        let lib = scratch.library();
        let voice = lib.join("voices").join("default-voice.json");

        std::fs::write(&voice, "{ not an instrument").unwrap();
        let broken = Tree::resolve(&lib.join("default.json"), None);
        assert_eq!(broken.documents().len(), 2, "still watched");
        assert_eq!(broken.blocked().len(), 1, "{:?}", broken.blocked());

        std::fs::remove_file(&voice).unwrap();
        let missing = Tree::resolve(&lib.join("default.json"), None);
        assert_eq!(missing.documents().len(), 2, "watched for its return");
        assert_eq!(missing.blocked(), Vec::<String>::new());
    }

    #[test]
    fn a_top_level_file_that_is_missing_blocks() {
        let scratch = Scratch::new("tree_missing_top");
        let tree = Tree::resolve(&scratch.path("nothing.json"), None);
        assert_eq!(tree.blocked().len(), 1);
        assert!(tree.top().hash().is_none());
    }
}
