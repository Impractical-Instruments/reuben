//! The native file dialogs. Each blocks the UI thread until the user answers, and returns `None`
//! when they cancel. No test can drive one, so callers take the path they return as an argument
//! and are tested with a path of their own.

use std::path::{Path, PathBuf};

/// Ask for an existing instrument document, starting in `dir`.
pub fn instrument_to_open(dir: Option<&Path>) -> Option<PathBuf> {
    dialog(dir).pick_file()
}

/// Ask for a WAV file to name as a sample resource, starting in `dir`.
pub fn sample(dir: Option<&Path>) -> Option<PathBuf> {
    in_dir(
        rfd::FileDialog::new().add_filter("Sample", &["wav", "WAV"]),
        dir,
    )
    .pick_file()
}

fn dialog(dir: Option<&Path>) -> rfd::FileDialog {
    in_dir(
        rfd::FileDialog::new().add_filter("Instrument", &["json"]),
        dir,
    )
}

fn in_dir(dialog: rfd::FileDialog, dir: Option<&Path>) -> rfd::FileDialog {
    match dir {
        Some(dir) => dialog.set_directory(dir),
        None => dialog,
    }
}
