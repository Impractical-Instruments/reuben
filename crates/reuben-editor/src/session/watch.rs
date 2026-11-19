//! The file watcher over the tree's files.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};

/// Watches a set of files and says when any of them was touched.
///
/// It watches each file's directory rather than the file. A document verb replaces a document by
/// renaming a temp file over it, which leaves a watch on the file itself attached to the file that
/// is gone; and a file that is not there yet has only a directory to watch. Where the directory is
/// not there either, the nearest one above it that is gets the watch, and the directory appearing
/// is the touch.
pub struct Watch {
    watcher: RecommendedWatcher,
    touched: Receiver<()>,
    files: Arc<Mutex<BTreeSet<PathBuf>>>,
    directories: BTreeSet<PathBuf>,
    /// The directories the last [`set`](Self::set) could not watch, and why.
    unwatched: Vec<String>,
}

impl Watch {
    /// `wake` is called, on the watcher's thread, after each touch is recorded.
    pub fn new(wake: impl Fn() + Send + 'static) -> Result<Self, String> {
        let (tx, touched) = mpsc::channel();
        let files = Arc::new(Mutex::new(BTreeSet::<PathBuf>::new()));
        let watched = Arc::clone(&files);
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            let ours = match event {
                // Reading a file is an event too on Linux, and every reload reads every file: a
                // read that counted as a touch would reload forever.
                Ok(event) if matches!(event.kind, EventKind::Access(_)) => false,
                // Events were lost or the watch broke: what changed is unknown, so look.
                Ok(event) if event.need_rescan() => true,
                Err(_) => true,
                Ok(event) => {
                    let files = watched.lock().unwrap_or_else(|e| e.into_inner());
                    // The file itself, or a directory above it that appeared, moved or went.
                    (event.paths.iter()).any(|path| files.iter().any(|file| file.starts_with(path)))
                }
            };
            if ours && tx.send(()).is_ok() {
                wake();
            }
        })
        .map_err(|e| e.to_string())?;
        Ok(Self {
            watcher,
            touched,
            files,
            directories: BTreeSet::new(),
            unwatched: Vec::new(),
        })
    }

    /// Watch exactly `paths`, which are absolute.
    pub fn set<'a>(&mut self, paths: impl IntoIterator<Item = &'a Path>) {
        let mut files = BTreeSet::new();
        let mut directories = BTreeSet::new();
        for path in paths {
            let Some(directory) = path.parent() else {
                continue;
            };
            files.insert(path.to_path_buf());
            // A backend that reports the path a symlinked directory resolves to has to find the
            // file under that spelling as well.
            if let (Ok(real), Some(name)) = (directory.canonicalize(), path.file_name()) {
                files.insert(real.join(name));
            }
            if let Some(there) = directory.ancestors().find(|above| above.is_dir()) {
                directories.insert(there.to_path_buf());
            }
        }
        *self.files.lock().unwrap_or_else(|e| e.into_inner()) = files;

        for gone in self.directories.difference(&directories) {
            let _ = self.watcher.unwatch(gone);
        }
        let mut watching = BTreeSet::new();
        self.unwatched.clear();
        for directory in directories {
            // Asked for again even when it was watched before: a directory removed and made again
            // is a new directory, and the watch on the old one went with it.
            match self.watcher.watch(&directory, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    watching.insert(directory);
                }
                Err(e) => self.unwatched.push(format!("{}: {e}", directory.display())),
            }
        }
        self.directories = watching;
    }

    /// The directories that are not being watched, each with why.
    pub fn unwatched(&self) -> &[String] {
        &self.unwatched
    }

    /// Whether a watched file was touched since the last call.
    pub fn touched(&self) -> bool {
        self.touched.try_iter().count() > 0
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;
    use crate::session::test_support::Scratch;

    fn touched_within(watch: &Watch, limit: Duration) -> bool {
        let deadline = Instant::now() + limit;
        while Instant::now() < deadline {
            if watch.touched() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    const SEEN: Duration = Duration::from_secs(5);
    const QUIET: Duration = Duration::from_millis(300);

    #[test]
    fn a_write_a_replace_and_a_first_appearance_are_each_a_touch() {
        let scratch = Scratch::new("watch_touch");
        let file = scratch.path("a.json");
        let later = scratch.path("later.json");
        std::fs::write(&file, "one").unwrap();
        let mut watch = Watch::new(|| {}).expect("a watcher");
        watch.set([file.as_path(), later.as_path()]);

        std::fs::write(&file, "two").unwrap();
        assert!(touched_within(&watch, SEEN), "a write in place");
        while touched_within(&watch, QUIET) {}

        let temp = scratch.path(".a.json.tmp");
        std::fs::write(&temp, "three").unwrap();
        std::fs::rename(&temp, &file).unwrap();
        assert!(touched_within(&watch, SEEN), "a rename over the file");
        while touched_within(&watch, QUIET) {}

        std::fs::write(&later, "new").unwrap();
        assert!(touched_within(&watch, SEEN), "a file that was not there");
    }

    #[test]
    fn a_directory_removed_and_made_again_is_a_touch_and_so_is_a_file_in_the_new_one() {
        let scratch = Scratch::new("watch_recreate");
        let dir = scratch.path("voices");
        let file = dir.join("a.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&file, "one").unwrap();
        let mut watch = Watch::new(|| {}).expect("a watcher");
        watch.set([file.as_path()]);

        std::fs::remove_dir_all(&dir).unwrap();
        assert!(touched_within(&watch, SEEN), "the directory going");
        watch.set([file.as_path()]);
        while touched_within(&watch, QUIET) {}

        std::fs::create_dir_all(&dir).unwrap();
        assert!(touched_within(&watch, SEEN), "the directory coming back");
        watch.set([file.as_path()]);
        while touched_within(&watch, QUIET) {}

        std::fs::write(&file, "two").unwrap();
        assert!(touched_within(&watch, SEEN), "a file in the new directory");
        assert!(watch.unwatched().is_empty());
    }

    #[test]
    fn a_read_and_a_neighbouring_file_are_not_touches() {
        let scratch = Scratch::new("watch_quiet");
        let file = scratch.path("a.json");
        std::fs::write(&file, "one").unwrap();
        let mut watch = Watch::new(|| {}).expect("a watcher");
        watch.set([file.as_path()]);

        std::fs::read_to_string(&file).unwrap();
        std::fs::write(scratch.path("b.json"), "other").unwrap();
        assert!(!touched_within(&watch, QUIET));
    }

    #[test]
    fn a_file_no_longer_in_the_set_is_not_a_touch() {
        let scratch = Scratch::new("watch_unset");
        let (a, b) = (
            scratch.path("one").join("a.json"),
            scratch.path("two").join("b.json"),
        );
        for file in [&a, &b] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "one").unwrap();
        }
        let mut watch = Watch::new(|| {}).expect("a watcher");
        watch.set([a.as_path()]);
        watch.set([b.as_path()]);

        std::fs::write(&a, "two").unwrap();
        assert!(!touched_within(&watch, QUIET));
        std::fs::write(&b, "two").unwrap();
        assert!(touched_within(&watch, SEEN));
    }
}
