//! The instruments the user opened last, most recent first, stored per user in `recent.json` under
//! the [`ConfigDir`]. Each is the server's name for it; a list stored as paths reads as names.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::Choice;
use crate::config::ConfigDir;

const FILE: &str = "recent.json";

/// How many names the list keeps. Opening one more drops the oldest.
pub const KEEP: usize = 10;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Stored {
    files: Vec<String>,
}

/// The recent names, and the directory they are stored in.
pub struct Recent {
    dir: ConfigDir,
    names: Vec<String>,
    /// What loading or the last save reported, for the start screen to show.
    pub warning: Option<String>,
}

impl Recent {
    /// Read the stored list. A missing file is an empty list; an unreadable one is an empty list
    /// and a warning.
    pub fn load(dir: ConfigDir) -> Self {
        let (stored, warning) = dir.load::<Stored>(FILE);
        let mut names = Vec::with_capacity(KEEP);
        for name in stored.files {
            if !names.contains(&name) && names.len() < KEEP {
                names.push(name);
            }
        }
        Self {
            dir,
            names,
            warning,
        }
    }

    /// Most recent first.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// Put `name` first and store the list.
    pub fn opened(&mut self, name: &str) {
        self.names.retain(|n| n != name);
        self.names.insert(0, name.to_string());
        self.names.truncate(KEEP);
        self.warning = self
            .dir
            .save(
                FILE,
                &Stored {
                    files: self.names.clone(),
                },
            )
            .err()
            .map(|e| format!("the recent instruments were not saved: {e}"));
    }

    /// Each recent name, and what choosing it does: opens it when `offered`, the server's `list
    /// instruments`, names it; else, when `embedded`, offers its directory to the editor's own
    /// server first, as "Open file…" does; else nothing, and the row is greyed. Every one is
    /// greyed while `offered` is not known.
    pub fn rows<'a>(
        &'a self,
        offered: Option<&[String]>,
        embedded: bool,
    ) -> Vec<(&'a str, Option<Choice>)> {
        (self.names.iter())
            .map(|name| {
                let choice = match offered {
                    Some(offered) if offered.contains(name) => Some(Choice::Open(name.clone())),
                    Some(_) if embedded => Some(Choice::OpenFile(PathBuf::from(name))),
                    _ => None,
                };
                (name.as_str(), choice)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::TempConfig;

    #[test]
    fn an_opened_file_comes_first_and_survives_a_reload() {
        let temp = TempConfig::new();
        let mut recent = Recent::load(temp.dir());
        assert!(recent.names().is_empty());
        recent.opened("/lib/a.json");
        recent.opened("/lib/b.json");
        assert_eq!(recent.warning, None);

        let reloaded = Recent::load(temp.dir());
        assert_eq!(reloaded.names(), ["/lib/b.json", "/lib/a.json"]);
        assert_eq!(reloaded.warning, None);
    }

    #[test]
    fn reopening_a_file_moves_it_to_the_front_without_a_second_entry() {
        let temp = TempConfig::new();
        let mut recent = Recent::load(temp.dir());
        for name in ["a", "b", "c", "a"] {
            recent.opened(&format!("/lib/{name}.json"));
        }
        assert_eq!(
            recent.names(),
            ["/lib/a.json", "/lib/c.json", "/lib/b.json"]
        );
    }

    #[test]
    fn the_oldest_file_drops_off_past_the_limit() {
        let temp = TempConfig::new();
        let mut recent = Recent::load(temp.dir());
        for i in 0..=KEEP {
            recent.opened(&format!("/lib/{i}.json"));
        }
        assert_eq!(recent.names().len(), KEEP);
        assert_eq!(recent.names()[0], format!("/lib/{KEEP}.json"));
        assert!(!recent.names().contains(&"/lib/0.json".to_string()));
    }

    #[test]
    fn a_hand_edited_list_is_cut_to_the_limit_and_deduplicated() {
        let temp = TempConfig::new();
        let mut files: Vec<_> = (0..KEEP + 5).map(|i| format!("/lib/{i}.json")).collect();
        files.insert(1, "/lib/0.json".to_string());
        temp.dir()
            .save(FILE, &Stored { files })
            .expect("seed the list");
        let recent = Recent::load(temp.dir());
        assert_eq!(recent.names().len(), KEEP);
        assert_eq!(recent.names()[1], "/lib/1.json");
    }

    #[test]
    fn a_list_stored_as_paths_reads_as_names() {
        let temp = TempConfig::new();
        let files = vec![std::path::PathBuf::from("/lib/a.json")];
        #[derive(Serialize)]
        struct Paths {
            files: Vec<std::path::PathBuf>,
        }
        temp.dir()
            .save(FILE, &Paths { files })
            .expect("seed the list");
        assert_eq!(Recent::load(temp.dir()).names(), ["/lib/a.json"]);
    }

    #[test]
    fn a_recent_name_the_server_no_longer_offers_is_greyed_unless_the_server_is_the_editors() {
        use reuben_native::test_support::within;

        use crate::engine::test_support::{silent, HANG_SECS};
        use crate::engine::Engine;
        use crate::session::test_support::Scratch;

        within(HANG_SECS, || {
            let temp = TempConfig::new();
            let scratch = Scratch::new("recent_greyed");
            let lib = scratch.library();
            let offered_name = lib.join("default.json").display().to_string();
            // Opened through "Open file…" in an earlier run: its directory is offered no longer.
            let elsewhere = scratch.path("elsewhere/mine.json");
            std::fs::create_dir_all(elsewhere.parent().unwrap()).unwrap();
            std::fs::copy(lib.join("default.json"), &elsewhere).unwrap();
            let elsewhere_name = elsewhere.display().to_string();
            let mut recent = Recent::load(temp.dir());
            recent.opened(&elsewhere_name);
            recent.opened(&offered_name);
            assert_eq!(
                recent.rows(None, true),
                [
                    (offered_name.as_str(), None),
                    (elsewhere_name.as_str(), None)
                ],
                "nothing opens before the server says what it offers"
            );

            let mut engine =
                Engine::start_with(silent(&lib), reuben_native::engine::start_headless);
            let offered = super::super::library::offered(engine.channel().expect("runs"), || {})
                .recv_timeout(std::time::Duration::from_secs(HANG_SECS))
                .expect("the worker answers")
                .expect("the server lists");
            let opens = Some(Choice::Open(offered_name.clone()));
            assert_eq!(
                recent.rows(Some(&offered), false),
                [
                    (offered_name.as_str(), opens.clone()),
                    (elsewhere_name.as_str(), None)
                ],
                "an attached editor cannot offer a directory"
            );
            let rows = recent.rows(Some(&offered), true);
            assert_eq!(rows[0], (offered_name.as_str(), opens));
            let Some(Choice::OpenFile(path)) = rows[1].1.clone() else {
                panic!("{rows:?}");
            };

            // What the editor does with an `OpenFile` choice.
            let name = super::super::offer_file(&path, &mut engine);
            assert_eq!(name, elsewhere_name);
            let session = super::super::open(&name, &engine, || {}).expect("it opens");
            assert_eq!(session.top(), elsewhere_name);
            engine.shutdown();
        });
    }

    #[test]
    fn a_corrupt_list_is_empty_with_a_warning() {
        let temp = TempConfig::new();
        std::fs::create_dir_all(&temp.path).expect("mkdir");
        std::fs::write(temp.path.join(FILE), "{ nope").expect("seed");
        let recent = Recent::load(temp.dir());
        assert!(recent.names().is_empty());
        assert!(recent.warning.is_some_and(|w| w.contains(FILE)));
    }
}
