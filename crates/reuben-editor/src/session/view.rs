//! The files of the open instrument, whether the engine is playing them, and undo and redo.

use std::path::Path;

use eframe::egui;

use super::{Direction, Document, Session, Status};

const UNDO: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);
const REDO: egui::KeyboardShortcut = egui::KeyboardShortcut::new(
    egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
    egui::Key::Z,
);
const REDO_ALT: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Y);

/// The undo or redo the keyboard asked for this frame, unless a text field has the keyboard and
/// its own undo is the one meant.
pub fn shortcut(ctx: &egui::Context) -> Option<Direction> {
    if ctx.egui_wants_keyboard_input() {
        return None;
    }
    ctx.input_mut(|input| {
        // Redo first: a shortcut match ignores an extra Shift, so Undo's would take it.
        if input.consume_shortcut(&REDO) || input.consume_shortcut(&REDO_ALT) {
            Some(Direction::Redo)
        } else if input.consume_shortcut(&UNDO) {
            Some(Direction::Undo)
        } else {
            None
        }
    })
}

/// Draw the session down the left of `ui`. Returns the undo or redo clicked.
pub fn show(ui: &mut egui::Ui, session: &Session) -> Option<Direction> {
    let mut asked = None;
    egui::Panel::left("session")
        .resizable(true)
        .default_size(260.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    asked = undo(ui, session);
                    ui.separator();
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
    asked
}

fn undo(ui: &mut egui::Ui, session: &Session) -> Option<Direction> {
    let mut asked = None;
    ui.horizontal(|ui| {
        let ctx = ui.ctx().clone();
        for (direction, label, shortcut) in [
            (Direction::Undo, "Undo", UNDO),
            (Direction::Redo, "Redo", REDO),
        ] {
            let enabled = session.undo().next(direction).is_some();
            let button = egui::Button::new(label).shortcut_text(ctx.format_shortcut(&shortcut));
            if ui.add_enabled(enabled, button).clicked() {
                asked = Some(direction);
            }
        }
    });
    if let Some(note) = session.undo().note() {
        ui.colored_label(ui.visuals().warn_fg_color, note);
    }
    asked
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
        Status::NoEngine => {
            ui.label("No engine is running; these files are not playing.");
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
