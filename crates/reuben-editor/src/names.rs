//! Lexical math on the names the server gives: what the editor may work out from a name without
//! asking the filesystem.

use std::path::{Component, Path, PathBuf};

/// `path` made absolute, with each `.` dropped and each `..` folded into the component before it,
/// as the resolver folds them: never through a symlink.
pub fn lexical(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut folded = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                folded.pop();
            }
            other => folded.push(other),
        }
    }
    folded
}
