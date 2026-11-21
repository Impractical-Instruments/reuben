//! The instruments the start screen offers to play: what the server's `library` answers, fetched
//! off the UI thread.

use std::path::Path;
use std::sync::mpsc::{self, Receiver};

use reuben_api::engine::wire::{Library, LibraryEntry, ListWhat};
use reuben_api::engine::Channel;

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The server's name for it, which opens it.
    pub name: String,
    /// The document's `instrument` name, or its file stem when it does not read.
    pub title: String,
    pub description: Option<String>,
    /// Why the document does not read; it is listed, and cannot be opened from the list.
    pub error: Option<String>,
}

impl From<&LibraryEntry> for Entry {
    fn from(entry: &LibraryEntry) -> Self {
        Self {
            name: entry.name.clone(),
            title: (entry.title.clone()).unwrap_or_else(|| stem(&entry.name)),
            description: entry.description.clone(),
            error: entry.error.clone(),
        }
    }
}

/// The file stem of the server name `name`, for display.
pub fn stem(name: &str) -> String {
    Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string())
}

/// Ask `channel`'s server for its library on a worker thread. `wake` is called from it once the
/// answer is in the receiver.
pub fn fetch(
    channel: Channel,
    wake: impl Fn() + Send + 'static,
) -> Receiver<Result<Library, String>> {
    on_worker(move || channel.library().map_err(|e| e.to_string()), wake)
}

/// Ask `channel`'s server for the instrument names it offers on a worker thread, as [`fetch`].
pub fn offered(
    channel: Channel,
    wake: impl Fn() + Send + 'static,
) -> Receiver<Result<Vec<String>, String>> {
    on_worker(
        move || (channel.list(ListWhat::Instruments)).map_err(|e| e.to_string()),
        wake,
    )
}

fn on_worker<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    wake: impl Fn() + Send + 'static,
) -> Receiver<T> {
    let (done, answer) = mpsc::channel();
    std::thread::spawn(move || {
        if done.send(work()).is_ok() {
            wake();
        }
    });
    answer
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use reuben_native::test_support::within;

    use super::*;
    use crate::engine::test_support::{silent, HANG_SECS};
    use crate::engine::Engine;
    use crate::session::test_support::Scratch;

    fn answer<T>(rx: Receiver<T>) -> T {
        rx.recv_timeout(Duration::from_secs(HANG_SECS))
            .expect("the worker answers")
    }

    #[test]
    fn the_start_screen_lists_the_servers_library() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("library_server");
            let lib = scratch.library();
            let mut engine =
                Engine::start_with(silent(&lib), reuben_native::engine::start_headless);
            let channel = engine.channel().expect("the server runs");
            let library = answer(fetch(channel, || {})).expect("the library");

            let mut listed: Vec<String> =
                library.instruments.iter().map(|e| e.name.clone()).collect();
            listed.sort();
            let mut files: Vec<String> = std::fs::read_dir(&lib)
                .expect("read the library")
                .flatten()
                .map(|item| item.path())
                .filter(|path| path.is_file() && path.extension().is_some_and(|e| e == "json"))
                .map(|path| std::path::absolute(path).unwrap().display().to_string())
                .collect();
            files.sort();
            assert_eq!(listed, files);
            let entries: Vec<Entry> = library.instruments.iter().map(Entry::from).collect();
            let titles: Vec<&str> = entries.iter().map(|e| e.title.as_str()).collect();
            assert!(titles.contains(&"groovebox"), "{titles:?}");
            assert!(entries.iter().all(|e| e.error.is_none()), "{entries:?}");
            assert!(
                library.dirs.contains(&lib.display().to_string()),
                "{:?}",
                library.dirs
            );
            engine.shutdown();
        });
    }

    #[test]
    fn a_document_that_does_not_read_is_listed_with_why() {
        within(HANG_SECS, || {
            let scratch = Scratch::new("library_unreadable");
            let dir = scratch.path("lib");
            std::fs::create_dir_all(&dir).expect("mkdir");
            std::fs::write(dir.join("broken.json"), "{ not json").expect("seed");
            std::fs::write(dir.join("notes.txt"), "not an instrument").expect("seed");
            std::fs::copy(
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instruments/patches/space.json"),
                dir.join("fx.json"),
            )
            .expect("seed");
            let mut engine =
                Engine::start_with(silent(&dir), reuben_native::engine::start_headless);
            let channel = engine.channel().expect("the server runs");
            let library = answer(fetch(channel, || {})).expect("the library");

            let entries: Vec<Entry> = library.instruments.iter().map(Entry::from).collect();
            let titles: Vec<&str> = entries.iter().map(|e| e.title.as_str()).collect();
            assert_eq!(titles, ["broken", "space"]);
            assert!(entries[0].error.is_some());
            assert_eq!(entries[1].error, None);
            assert!(entries[1].description.is_some());
            engine.shutdown();
        });
    }
}
