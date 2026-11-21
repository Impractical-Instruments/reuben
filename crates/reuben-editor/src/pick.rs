//! The native file dialog behind "Open file…". It blocks the UI thread until the user answers, and
//! returns `None` when they cancel. No test can drive it, so its caller takes the path it returns
//! as an argument and is tested with a path of its own.

use std::path::{Path, PathBuf};

/// Ask for an existing instrument document, starting in `dir`.
pub fn instrument_to_open(dir: Option<&Path>) -> Option<PathBuf> {
    let dialog = rfd::FileDialog::new().add_filter("Instrument", &["json"]);
    match dir {
        Some(dir) => dialog.set_directory(dir),
        None => dialog,
    }
    .pick_file()
}
