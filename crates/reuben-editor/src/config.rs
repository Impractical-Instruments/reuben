//! Per-user editor state: small JSON documents under `$XDG_CONFIG_HOME/reuben-editor/`, or
//! `~/.config/reuben-editor/` when that is unset. One file per concern, so a broken or
//! newer-format file costs only what that file held.
//!
//! Nothing here can stop the editor: a missing file is the default value, an unreadable one is the
//! default value and a warning to show, and a failed save is an error for the caller to show.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;

const APP_DIR: &str = "reuben-editor";

/// Where the editor's per-user state lives.
#[derive(Debug, Clone)]
pub struct ConfigDir {
    /// `None` when the environment names no home to put one under: every load is the default and
    /// every save fails.
    dir: Option<PathBuf>,
}

impl ConfigDir {
    /// The user's directory, from `XDG_CONFIG_HOME` and `HOME`.
    pub fn user() -> Self {
        Self::from_env(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }

    /// The XDG base directory rule: `xdg_config_home` when it is an absolute path (the
    /// specification says to ignore a relative one), else `home/.config`. A relative `home` names no
    /// directory either: it would follow the working directory.
    fn from_env(xdg_config_home: Option<OsString>, home: Option<OsString>) -> Self {
        let base = xdg_config_home
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| {
                home.map(PathBuf::from)
                    .filter(|h| h.is_absolute())
                    .map(|h| h.join(".config"))
            });
        Self {
            dir: base.map(|b| b.join(APP_DIR)),
        }
    }

    /// `dir` itself, with no `reuben-editor/` appended: a test's temporary directory.
    #[cfg(test)]
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
        }
    }

    /// Where `file` is, or would be, stored.
    pub fn path(&self, file: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join(file))
    }

    /// Read `file`. A missing file (or no directory at all) is `T::default()`. A file that cannot
    /// be read or parsed is `T::default()` too, with a warning saying so; the file is left as it
    /// is until the next [`save`](Self::save) replaces it.
    pub fn load<T: DeserializeOwned + Default>(&self, file: &str) -> (T, Option<String>) {
        let Some(path) = self.path(file) else {
            return (T::default(), None);
        };
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return (T::default(), None),
            Err(e) => return (T::default(), Some(ignored(&path, &e))),
        };
        match serde_json::from_str(&text) {
            Ok(value) => (value, None),
            Err(e) => (T::default(), Some(ignored(&path, &e))),
        }
    }

    /// Write `file`, creating the directory if it is missing. The new content replaces the old in
    /// one rename, so a crash mid-save leaves the old file rather than a truncated one.
    pub fn save<T: Serialize>(&self, file: &str, value: &T) -> io::Result<()> {
        let Some(dir) = &self.dir else {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "no config directory: neither XDG_CONFIG_HOME nor HOME names one",
            ));
        };
        std::fs::create_dir_all(dir)?;
        let mut text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
        text.push('\n');
        // The pid keeps two editors saving at once off each other's scratch file.
        let scratch = dir.join(format!(".{file}.{}.tmp", std::process::id()));
        let saved =
            std::fs::write(&scratch, text).and_then(|()| std::fs::rename(&scratch, dir.join(file)));
        if saved.is_err() {
            let _ = std::fs::remove_file(&scratch);
        }
        saved
    }
}

fn ignored(path: &Path, error: &dyn std::fmt::Display) -> String {
    format!("ignoring {}: {error}", path.display())
}

#[cfg(test)]
pub mod test_support {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::ConfigDir;

    /// A directory of its own under the system temp directory, removed on drop.
    pub struct TempConfig {
        pub path: PathBuf,
    }

    impl TempConfig {
        pub fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "reuben_editor_config_{}_{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            Self { path }
        }

        pub fn dir(&self) -> ConfigDir {
            ConfigDir::at(&self.path)
        }
    }

    impl Drop for TempConfig {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::test_support::TempConfig;
    use super::*;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    struct Doc {
        name: Option<String>,
        count: u32,
    }

    fn doc() -> Doc {
        Doc {
            name: Some("Scarlett".to_string()),
            count: 3,
        }
    }

    #[test]
    fn a_saved_document_loads_back() {
        let temp = TempConfig::new();
        temp.dir().save("doc.json", &doc()).expect("save");
        assert_eq!(temp.dir().load::<Doc>("doc.json"), (doc(), None));
    }

    #[test]
    fn saving_creates_the_directory_and_leaves_no_scratch_file() {
        let temp = TempConfig::new();
        let nested = ConfigDir::at(temp.path.join("a/b"));
        nested.save("doc.json", &doc()).expect("save");
        let files: Vec<_> = std::fs::read_dir(temp.path.join("a/b"))
            .expect("the directory exists")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(files, ["doc.json"]);
    }

    #[test]
    fn a_missing_file_is_the_default_without_a_warning() {
        let temp = TempConfig::new();
        assert_eq!(temp.dir().load::<Doc>("doc.json"), (Doc::default(), None));
    }

    #[test]
    fn a_corrupt_file_is_the_default_with_a_warning_and_the_next_save_replaces_it() {
        let temp = TempConfig::new();
        std::fs::create_dir_all(&temp.path).expect("mkdir");
        for corrupt in ["{ not json", r#"{"count": "three"}"#, ""] {
            std::fs::write(temp.path.join("doc.json"), corrupt).expect("seed");
            let (loaded, warning) = temp.dir().load::<Doc>("doc.json");
            assert_eq!(loaded, Doc::default(), "{corrupt:?}");
            let warning = warning.expect("a warning");
            assert!(warning.contains("doc.json"), "{warning}");
        }
        temp.dir().save("doc.json", &doc()).expect("save");
        assert_eq!(temp.dir().load::<Doc>("doc.json"), (doc(), None));
    }

    #[test]
    fn the_directory_follows_the_xdg_rule() {
        let dir = |xdg: Option<&str>, home: Option<&str>| {
            ConfigDir::from_env(xdg.map(OsString::from), home.map(OsString::from))
                .path("audio.json")
        };
        assert_eq!(
            dir(Some("/xdg"), Some("/home/u")),
            Some(PathBuf::from("/xdg/reuben-editor/audio.json"))
        );
        assert_eq!(
            dir(None, Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/reuben-editor/audio.json"))
        );
        assert_eq!(
            dir(Some("relative"), Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/reuben-editor/audio.json"))
        );
        assert_eq!(
            dir(Some(""), Some("/home/u")),
            Some(PathBuf::from("/home/u/.config/reuben-editor/audio.json"))
        );
        assert_eq!(dir(None, None), None);
        assert_eq!(dir(None, Some("relative/home")), None);
    }

    #[test]
    fn with_no_directory_loading_is_the_default_and_saving_is_an_error() {
        let nowhere = ConfigDir::from_env(None, None);
        assert_eq!(nowhere.load::<Doc>("doc.json"), (Doc::default(), None));
        assert!(nowhere.save("doc.json", &doc()).is_err());
    }
}
