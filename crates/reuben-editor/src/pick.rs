//! The native file dialogs. Each blocks the UI thread until the user answers, and returns `None`
//! when they cancel. No test can drive one, so callers take the path they return as an argument
//! and are tested with a path of their own.

use std::path::{Path, PathBuf};

/// Ask for an existing instrument document, starting in `dir`.
pub fn instrument_to_open(dir: Option<&Path>) -> Option<PathBuf> {
    dialog(dir).pick_file()
}

/// Ask where a new instrument document goes, starting in `dir`.
pub fn instrument_to_create(dir: Option<&Path>) -> Option<PathBuf> {
    let path = dialog(dir).set_file_name("untitled.json").save_file()?;
    Some(with_json_extension(path))
}

/// `path` ending in `.json`, so the library lists the document made there. Appended rather than
/// set: `my.synth` becomes `my.synth.json`, not `my.json`.
fn with_json_extension(path: PathBuf) -> PathBuf {
    if path.extension().is_some_and(|ext| ext == "json") {
        return path;
    }
    let mut name = path.into_os_string();
    name.push(".json");
    PathBuf::from(name)
}

fn dialog(dir: Option<&Path>) -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new().add_filter("Instrument", &["json"]);
    match dir {
        Some(dir) => dialog.set_directory(dir),
        None => dialog,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_document_always_ends_in_json() {
        for (typed, made) in [
            ("/lib/pad", "/lib/pad.json"),
            ("/lib/pad.json", "/lib/pad.json"),
            ("/lib/my.synth", "/lib/my.synth.json"),
        ] {
            assert_eq!(
                with_json_extension(PathBuf::from(typed)),
                PathBuf::from(made)
            );
        }
    }
}
