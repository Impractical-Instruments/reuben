//! The files of the open instrument and whether the engine is playing them.

use std::path::Path;

use eframe::egui;

use super::{Document, Session, Status};

/// Draw the session down the left of `ui`.
pub fn show(ui: &mut egui::Ui, session: &Session) {
    egui::Panel::left("session")
        .resizable(true)
        .default_size(260.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    ui.strong("Files");
                    let beside = session.tree().top().path.parent();
                    for document in session.tree().documents() {
                        file(ui, document, beside);
                    }
                    ui.separator();
                    status(ui, session.status());
                    if let Some(e) = session.watch_error() {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!("Not watching these files for changes: {e}"),
                        );
                    }
                });
        });
}

fn file(ui: &mut egui::Ui, document: &Document, beside: Option<&Path>) {
    let name = label(&document.path, beside);
    let line = match &document.read {
        Ok(structure) => ui.label(format!("{name} — {} node(s)", structure.nodes.len())),
        Err(_) => ui.colored_label(ui.visuals().error_fg_color, format!("{name} — unreadable")),
    };
    line.on_hover_text(match &document.read {
        Ok(_) => document.path.display().to_string(),
        Err(e) => e.clone(),
    });
}

/// `path` as it reads from the top-level file's directory, or whole when it is not under it.
fn label(path: &Path, beside: Option<&Path>) -> String {
    beside
        .and_then(|dir| path.strip_prefix(dir).ok())
        .unwrap_or(path)
        .display()
        .to_string()
}

fn status(ui: &mut egui::Ui, status: &Status) {
    match status {
        Status::Current => {
            ui.label("Playing these files.");
        }
        Status::Swapping => {
            ui.label("Swapping…");
        }
        Status::Held(why) => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Still playing the last instrument that loaded:",
            );
            for line in why {
                ui.colored_label(ui.visuals().error_fg_color, line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn a_file_is_named_from_the_top_level_files_directory() {
        let lib = PathBuf::from("lib");
        let voice = lib.join("voices").join("kick.json");
        assert_eq!(
            Path::new(&label(&voice, Some(&lib))),
            Path::new("voices").join("kick.json")
        );
        let outside = PathBuf::from("shared").join("kick.json");
        assert_eq!(Path::new(&label(&outside, Some(&lib))), outside);
    }
}
