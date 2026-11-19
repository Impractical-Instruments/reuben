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
    // A name typed without the extension is still a document the library lists.
    Some(match path.extension() {
        Some(_) => path,
        None => path.with_extension("json"),
    })
}

fn dialog(dir: Option<&Path>) -> rfd::FileDialog {
    let dialog = rfd::FileDialog::new().add_filter("Instrument", &["json"]);
    match dir {
        Some(dir) => dialog.set_directory(dir),
        None => dialog,
    }
}
