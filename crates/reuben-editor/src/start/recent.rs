//! The instruments the user opened last, most recent first, stored per user in `recent.json` under
//! the [`ConfigDir`].

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::ConfigDir;

const FILE: &str = "recent.json";

/// How many files the list keeps. Opening one more drops the oldest.
pub const KEEP: usize = 10;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Stored {
    files: Vec<PathBuf>,
}

/// The recent files, and the directory they are stored in.
pub struct Recent {
    dir: ConfigDir,
    files: Vec<PathBuf>,
    /// What loading or the last save reported, for the start screen to show.
    pub warning: Option<String>,
}

impl Recent {
    /// Read the stored list. A missing file is an empty list; an unreadable one is an empty list
    /// and a warning.
    pub fn load(dir: ConfigDir) -> Self {
        let (stored, warning) = dir.load::<Stored>(FILE);
        let mut files = Vec::with_capacity(KEEP);
        for file in stored.files {
            if !files.contains(&file) && files.len() < KEEP {
                files.push(file);
            }
        }
        Self {
            dir,
            files,
            warning,
        }
    }

    /// Most recent first.
    pub fn files(&self) -> &[PathBuf] {
        &self.files
    }

    /// Put `path` first and store the list. `path` is made absolute first, so a file opened by a
    /// path relative to one working directory is found again from another.
    pub fn opened(&mut self, path: &Path) {
        let path = absolute(path);
        self.files.retain(|f| f != &path);
        self.files.insert(0, path);
        self.files.truncate(KEEP);
        self.warning = self
            .dir
            .save(
                FILE,
                &Stored {
                    files: self.files.clone(),
                },
            )
            .err()
            .map(|e| format!("the recent files were not saved: {e}"));
    }
}

/// `path` with symlinks and `..` resolved when it exists, else joined onto the working directory.
pub fn absolute(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::TempConfig;

    #[test]
    fn an_opened_file_comes_first_and_survives_a_reload() {
        let temp = TempConfig::new();
        let mut recent = Recent::load(temp.dir());
        assert!(recent.files().is_empty());
        recent.opened(Path::new("/lib/a.json"));
        recent.opened(Path::new("/lib/b.json"));
        assert_eq!(recent.warning, None);

        let reloaded = Recent::load(temp.dir());
        assert_eq!(
            reloaded.files(),
            [PathBuf::from("/lib/b.json"), PathBuf::from("/lib/a.json")]
        );
        assert_eq!(reloaded.warning, None);
    }

    #[test]
    fn reopening_a_file_moves_it_to_the_front_without_a_second_entry() {
        let temp = TempConfig::new();
        let mut recent = Recent::load(temp.dir());
        for name in ["a", "b", "c", "a"] {
            recent.opened(&PathBuf::from(format!("/lib/{name}.json")));
        }
        assert_eq!(
            recent.files(),
            ["/lib/a.json", "/lib/c.json", "/lib/b.json"].map(PathBuf::from)
        );
    }

    #[test]
    fn the_oldest_file_drops_off_past_the_limit() {
        let temp = TempConfig::new();
        let mut recent = Recent::load(temp.dir());
        for i in 0..=KEEP {
            recent.opened(&PathBuf::from(format!("/lib/{i}.json")));
        }
        assert_eq!(recent.files().len(), KEEP);
        assert_eq!(
            recent.files()[0],
            PathBuf::from(format!("/lib/{KEEP}.json"))
        );
        assert!(!recent.files().contains(&PathBuf::from("/lib/0.json")));
    }

    #[test]
    fn a_relative_path_is_stored_absolute() {
        let temp = TempConfig::new();
        let mut recent = Recent::load(temp.dir());
        recent.opened(Path::new("no-such-dir/x.json"));
        let stored = &recent.files()[0];
        assert!(stored.is_absolute(), "{}", stored.display());
        assert!(stored.ends_with("no-such-dir/x.json"));
    }

    #[test]
    fn a_hand_edited_list_is_cut_to_the_limit_and_deduplicated() {
        let temp = TempConfig::new();
        let mut files: Vec<_> = (0..KEEP + 5)
            .map(|i| PathBuf::from(format!("/lib/{i}.json")))
            .collect();
        files.insert(1, PathBuf::from("/lib/0.json"));
        temp.dir()
            .save(FILE, &Stored { files })
            .expect("seed the list");
        let recent = Recent::load(temp.dir());
        assert_eq!(recent.files().len(), KEEP);
        assert_eq!(recent.files()[1], PathBuf::from("/lib/1.json"));
    }

    #[test]
    fn a_corrupt_list_is_empty_with_a_warning() {
        let temp = TempConfig::new();
        std::fs::create_dir_all(&temp.path).expect("mkdir");
        std::fs::write(temp.path.join(FILE), "{ nope").expect("seed");
        let recent = Recent::load(temp.dir());
        assert!(recent.files().is_empty());
        assert!(recent.warning.is_some_and(|w| w.contains(FILE)));
    }
}
