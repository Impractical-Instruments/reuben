//! The instruments the start screen offers to play: the documents directly in the library
//! directory. The voices and patches the library keeps in its subdirectories are parts of those
//! instruments, not instruments to play on their own, so they are not listed.

use std::path::{Path, PathBuf};

use reuben_api::authoring::{read_instrument, ReadInstrument};
use reuben_api::FsResolver;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    /// The document's `instrument` name, or its file name when it does not read.
    pub name: String,
    pub description: Option<String>,
    /// Why the document does not read; it is listed, and cannot be opened from the list.
    pub error: Option<String>,
}

/// The library directory: `root` when one is given, else `instruments/` in the working directory
/// when there is one. Absolute, so what is opened from it is named the same from anywhere. `Err`
/// says, for the start screen, why there is none.
pub fn locate(root: Option<&Path>) -> Result<PathBuf, String> {
    match root {
        Some(root) if root.is_dir() => Ok(super::recent::absolute(root)),
        Some(root) => Err(format!(
            "No library: the instrument root {} is not a directory.",
            root.display()
        )),
        None if Path::new("instruments").is_dir() => {
            Ok(super::recent::absolute(Path::new("instruments")))
        }
        None => Err("No library: there is no instruments/ here. Name one with \
                     --instrument-root or REUBEN_INSTRUMENT_ROOT."
            .to_string()),
    }
}

/// Every `*.json` document directly in `dir`, by name. `root` is the library root references fall
/// back to, as for the engine.
pub fn scan(dir: &Path, root: Option<&Path>) -> Result<Vec<Entry>, String> {
    let listing = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let mut entries = Vec::new();
    // An entry the directory cannot report names no file to list, and costs only that entry.
    for item in listing.flatten() {
        let path = item.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "json") {
            entries.push(entry(path, root));
        }
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
    Ok(entries)
}

fn entry(path: PathBuf, root: Option<&Path>) -> Entry {
    let source = path.display().to_string();
    let mut store = FsResolver::for_document(&source).stat_only();
    if let Some(root) = root {
        store = store.with_root(root);
    }
    match read_instrument(&ReadInstrument { source }, &store) {
        Ok(answer) => Entry {
            name: answer.output.instrument,
            description: answer.output.description,
            error: None,
            path,
        },
        Err(refusal) => Entry {
            name: path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            description: None,
            error: Some(refusal.message),
            path,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::Scratch;

    #[test]
    fn the_repo_library_lists_its_top_level_instruments_by_name() {
        let lib = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instruments");
        let entries = scan(&lib, None).expect("scan the library");
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"default"), "{names:?}");
        assert!(names.contains(&"groovebox"), "{names:?}");
        assert!(
            !names.contains(&"default-voice"),
            "a voice is part of an instrument: {names:?}"
        );
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        for entry in &entries {
            assert_eq!(entry.error, None, "{}", entry.path.display());
        }
    }

    #[test]
    fn a_document_that_does_not_read_is_listed_with_why() {
        let scratch = Scratch::new("library_unreadable");
        let dir = scratch.path("");
        std::fs::write(dir.join("broken.json"), "{ not json").expect("seed");
        std::fs::write(dir.join("notes.txt"), "not an instrument").expect("seed");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instruments/patches/space.json"),
            dir.join("fx.json"),
        )
        .expect("seed");

        let entries = scan(&dir, None).expect("scan");
        let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["broken", "space"]);
        assert!(entries[0].error.is_some());
        assert_eq!(entries[1].error, None);
        assert!(entries[1].description.is_some());
    }

    #[test]
    fn the_library_is_the_root_when_one_is_given() {
        let scratch = Scratch::new("library_locate");
        let dir = scratch.path("lib");
        let missing = locate(Some(&dir)).expect_err("a root that is not a directory");
        assert!(missing.contains(&dir.display().to_string()), "{missing}");
        std::fs::create_dir_all(&dir).expect("mkdir");
        let located = locate(Some(&dir)).expect("a directory");
        assert!(located.is_absolute());
        assert_eq!(
            std::fs::canonicalize(located).ok(),
            std::fs::canonicalize(&dir).ok()
        );
    }
}
