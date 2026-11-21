//! Which instruments play a given document, so an edit to a voice or patch that more than one of them
//! shares can say so before it is made.

use std::path::Path;

use reuben_api::engine::wire::Library;

/// The names of the instruments of `library` whose tree reads the document the server names
/// `source`, sorted. An entry that is `source` itself is the document, not a user of it.
///
/// An instrument is named by its `instrument` name, or by its file's stem when it has none.
pub fn users(library: &Library, source: &str) -> Vec<String> {
    let mut names: Vec<String> = (library.instruments.iter())
        .filter(|entry| entry.name != source && entry.reads.iter().any(|read| read == source))
        .map(|entry| {
            entry.title.clone().unwrap_or_else(|| {
                let stem = Path::new(&entry.name).file_stem().unwrap_or_default();
                stem.to_string_lossy().into_owned()
            })
        })
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use reuben_api::engine::wire::LibraryEntry;
    use reuben_native::test_support::within;

    use super::*;
    use crate::session::test_support::Rig;

    const HANG_SECS: u64 = 60;

    /// Who the server's library says uses `relative`, a file of the rig's library.
    fn served_users(rig: &Rig, relative: &str) -> Vec<String> {
        let library = rig
            .channel()
            .library()
            .expect("the server lists its library");
        let source = (rig.session.tree().documents().iter())
            .map(|d| d.source.clone())
            .find(|source| crate::session::test_support::names(source, &rig.lib.join(relative)))
            .unwrap_or_else(|| panic!("{relative} is in the tree"));
        users(&library, &source)
    }

    #[test]
    fn users_are_the_library_instruments_whose_tree_reads_it() {
        within(HANG_SECS, || {
            let rig = Rig::open("users_kick", "acid-techno.json");
            let drums = ["acid-techno", "euclidean-drums", "groovebox"];
            assert_eq!(served_users(&rig, "voices/kick-voice.json"), drums);
            // Reached only through a drum voice, and counted once however many of them do.
            assert_eq!(served_users(&rig, "voices/shaped-vca.json"), drums);
        });
    }

    #[test]
    fn a_voice_one_instrument_uses_is_used_by_one() {
        within(HANG_SECS, || {
            let rig = Rig::open("users_one", "default.json");
            assert_eq!(served_users(&rig, "voices/default-voice.json"), ["default"]);
        });
    }

    #[test]
    fn the_document_itself_and_an_entry_that_does_not_read_are_no_users() {
        let entry = |name: &str, title: Option<&str>, reads: &[&str]| LibraryEntry {
            name: name.to_string(),
            title: title.map(str::to_string),
            description: None,
            error: None,
            reads: reads.iter().map(|r| r.to_string()).collect(),
        };
        let library = Library {
            instruments: vec![
                entry("/l/voice.json", Some("voice"), &["/l/voice.json"]),
                entry("/l/b.json", Some("b"), &["/l/b.json", "/l/voice.json"]),
                entry("/l/a.json", None, &["/l/a.json", "/l/voice.json"]),
                entry("/l/broken.json", None, &[]),
            ],
            dirs: Vec::new(),
        };
        assert_eq!(users(&library, "/l/voice.json"), ["a", "b"]);
        assert!(users(&library, "/l/elsewhere.json").is_empty());
    }
}
