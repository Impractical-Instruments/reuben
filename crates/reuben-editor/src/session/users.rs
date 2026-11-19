//! Which instruments play a given file, so an edit to a voice or patch that more than one of them
//! shares can say so before it is made.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::Tree;

/// The names of the instruments directly in any of `libraries` whose tree reaches the document at
/// `path`, sorted. `root` is the library root references fall back to, as for the engine. A
/// directory that cannot be listed is passed over, and is the answer only when none can be.
///
/// An instrument counts once however many of its nodes reach the file, or however many of
/// `libraries` hold it, and one that is the file itself does not count: it is the file, not a user
/// of it.
pub fn users(libraries: &[&Path], root: Option<&Path>, path: &Path) -> Result<Vec<String>, String> {
    let target = same_file(path);
    let mut tops = BTreeSet::new();
    let mut unlisted = Vec::new();
    for library in libraries {
        let listing = match std::fs::read_dir(library) {
            Ok(listing) => listing,
            Err(e) => {
                unlisted.push(format!("{}: {e}", library.display()));
                continue;
            }
        };
        // An entry the directory cannot report names no instrument, and costs only that entry.
        for entry in listing.flatten() {
            let top = entry.path();
            if top.is_file() && top.extension().is_some_and(|ext| ext == "json") {
                tops.insert(same_file(&top));
            }
        }
    }
    if unlisted.len() == libraries.len() {
        return Err(unlisted.join("; "));
    }
    let mut names = Vec::new();
    for top in tops {
        let tree = Tree::resolve(&top, root);
        let mut nested = tree.documents()[1..].iter();
        if nested.any(|document| same_file(&document.path) == target) {
            names.push(tree.top().name());
        }
    }
    names.sort();
    Ok(names)
}

/// `path` as one file is spelled however it was reached: a reference resolved from a relative
/// top-level path and one resolved from an absolute one name the same file.
fn same_file(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::test_support::Scratch;

    #[test]
    fn the_kick_voice_is_used_by_the_three_drum_instruments() {
        let scratch = Scratch::new("users_kick");
        let lib = scratch.library();
        let kick = lib.join("voices").join("kick-voice.json");
        assert_eq!(
            users(&[&lib], None, &kick).expect("the library lists"),
            ["acid-techno", "euclidean-drums", "groovebox"]
        );
    }

    /// `shaped-vca` is no top-level instrument's direct reference: each reaches it through a drum
    /// voice, and counts once for however many of its voices do.
    #[test]
    fn a_file_reached_through_another_counts_each_instrument_once() {
        let scratch = Scratch::new("users_nested");
        let lib = scratch.library();
        let vca = lib.join("voices").join("shaped-vca.json");
        assert_eq!(
            users(&[&lib], None, &vca).expect("the library lists"),
            ["acid-techno", "euclidean-drums", "groovebox"]
        );
    }

    #[test]
    fn a_voice_one_instrument_uses_is_used_by_one() {
        let scratch = Scratch::new("users_one");
        let lib = scratch.library();
        let voice = lib.join("voices").join("default-voice.json");
        assert_eq!(
            users(&[&lib], None, &voice).expect("the library lists"),
            ["default"]
        );
    }

    /// A copy of an instrument outside the library, playing a library voice through the root, is
    /// listed with the library's own users of that voice.
    #[test]
    fn the_instruments_beside_the_open_one_and_in_the_root_are_both_counted() {
        let scratch = Scratch::new("users_both");
        let lib = scratch.library();
        let mine = scratch.path("mine");
        std::fs::create_dir_all(&mine).unwrap();
        let text = std::fs::read_to_string(lib.join("default.json"))
            .unwrap()
            .replace(
                "\"instrument\": \"default\"",
                "\"instrument\": \"my-default\"",
            );
        std::fs::write(mine.join("my-default.json"), text).unwrap();
        let voice = lib.join("voices").join("default-voice.json");
        assert_eq!(
            users(&[&mine, &lib, &lib], Some(&lib), &voice).expect("both list"),
            ["default", "my-default"]
        );
    }

    #[test]
    fn a_root_that_is_not_there_costs_only_itself() {
        let scratch = Scratch::new("users_one_missing");
        let lib = scratch.library();
        let kick = lib.join("voices").join("kick-voice.json");
        let nothing = scratch.path("nothing");
        assert_eq!(
            users(&[&lib, &nothing], Some(&nothing), &kick).expect("the library lists"),
            ["acid-techno", "euclidean-drums", "groovebox"]
        );
    }

    #[test]
    fn a_library_that_is_not_there_says_why() {
        let scratch = Scratch::new("users_missing");
        assert!(users(&[&scratch.path("nothing")], None, &scratch.path("x.json")).is_err());
    }
}
