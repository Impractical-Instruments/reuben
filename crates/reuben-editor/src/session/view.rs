//! Undo and redo, whether the engine is playing the open instrument, and the way down into it.

use eframe::egui;
use reuben_api::engine::Channel;

use super::{Direction, Document, Listing, Session, Status};

const UNDO: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);
const REDO: egui::KeyboardShortcut = egui::KeyboardShortcut::new(
    egui::Modifiers::COMMAND.plus(egui::Modifiers::SHIFT),
    egui::Key::Z,
);
const REDO_ALT: egui::KeyboardShortcut =
    egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Y);

/// The most of the top bar the reasons an instrument is held may take, about six lines of them;
/// the rest scroll.
const HELD_HEIGHT: f32 = 110.0;

/// The undo or redo the keyboard asked for this frame, unless a text field has the keyboard and
/// its own undo is the one meant.
pub fn shortcut(ctx: &egui::Context) -> Option<Direction> {
    if ctx.text_edit_focused() {
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

/// What the trail was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailAsk {
    /// View the document this far down the trail: 0 is the top-level document.
    Leave(usize),
    /// Give the viewed document a file of its own for the document one step up.
    LocalCopy,
}

/// Draw the way down from the top-level document to the one viewed, as a line of links back up,
/// the local copy action, and, when the document viewed is one other instruments play too, who
/// they are, as the server's library says through `channel`.
pub fn trail(
    ui: &mut egui::Ui,
    session: &mut Session,
    channel: &dyn Fn() -> Option<Channel>,
) -> Option<TrailAsk> {
    if session.trail().is_empty() {
        return None;
    }
    let mut asked = None;
    ui.horizontal_wrapped(|ui| {
        let tree = session.tree();
        let names: Vec<String> = std::iter::once(tree.top().name())
            .chain((session.trail().iter()).map(|crumb| {
                (tree.document(&crumb.source)).map_or_else(|| crumb.source.clone(), Document::name)
            }))
            .collect();
        let last = names.len() - 1;
        for (depth, name) in names.iter().enumerate() {
            if depth > 0 {
                let crumb = &session.trail()[depth - 1];
                ui.weak("›");
                ui.weak(crumb.address.trim_start_matches('/'));
                ui.weak("›");
            }
            if depth == last {
                ui.strong(name);
            } else if ui.link(name).clicked() {
                asked = Some(TrailAsk::Leave(depth));
            }
        }
        let above = names[last - 1].clone();
        let copy = ui.button("Make a local copy").on_hover_text(format!(
            "Copy this file beside itself and point {above}'s resources entry for it at the copy. \
             Every node of {above} using that entry, and every instrument that plays {above}, \
             follows it; the original is left as it is. Undo points the entry back; the copy \
             stays on disk."
        ));
        if copy.clicked() {
            asked = Some(TrailAsk::LocalCopy);
        }
    });
    let viewed = std::path::Path::new(&session.viewed().source).to_path_buf();
    match session.users(channel) {
        Some(Listing::Listed(Ok(users))) if users.len() > 1 => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!(
                    "{} is used by {} instruments ({}): an edit here changes every one of them. \
                     Make a local copy to change this one only.",
                    viewed.file_name().unwrap_or_default().to_string_lossy(),
                    users.len(),
                    users.join(", ")
                ),
            );
        }
        Some(Listing::Checking) => {
            ui.weak("Checking which other instruments use this file…");
        }
        Some(Listing::Listed(Err(e))) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("Could not tell which other instruments use this file: {e}"),
            );
        }
        _ => {}
    }
    asked
}

/// Draw the session's rows of the top bar: undo and redo, and whether the engine plays the
/// instrument. Returns the undo or redo clicked.
pub fn bar(ui: &mut egui::Ui, session: &Session) -> Option<Direction> {
    let asked = undo(ui, session);
    let name = session.tree().top().name();
    status(ui, session.status(), &name);
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

fn status(ui: &mut egui::Ui, status: &Status, name: &str) {
    match status {
        Status::Current => {
            ui.label(format!("Playing {name}."));
        }
        Status::NoEngine => {
            ui.label(format!("No engine is running; {name} is not playing."));
        }
        Status::Held(why) => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                format!("{name} does not play as it is:"),
            );
            // One line per fault in every file of the instrument: unbounded, in a bar that takes
            // its height from the canvas below.
            // Salted with the reasons, so a new set opens at its first line rather than where the
            // last was scrolled to. The minimum is set too: a row of the top bar offers one line
            // of height, and the area would otherwise fall back to egui's smaller default.
            egui::ScrollArea::vertical()
                .id_salt(("held", why))
                .max_height(HELD_HEIGHT)
                .min_scrolled_height(HELD_HEIGHT)
                .show(ui, |ui| {
                    for line in why {
                        ui.colored_label(ui.visuals().error_fg_color, line);
                    }
                });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Press Ctrl+Z with the keyboard focus on the widget `widget` draws, and return what
    /// [`shortcut`] made of it. Asked before the widget is drawn, as the window asks.
    fn ctrl_z_with_focus_on(
        mut widget: impl FnMut(&mut egui::Ui) -> egui::Response,
    ) -> Option<Direction> {
        let ctx = egui::Context::default();
        // Nothing here paints, and the font atlas it would upload refuses a silent drop.
        ctx.run_ui(egui::RawInput::default(), |ui| widget(ui).request_focus())
            .drop_without_applying_deltas();
        let press = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Z,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            }],
            ..Default::default()
        };
        let mut asked = None;
        ctx.run_ui(press, |ui| {
            asked = shortcut(ui.ctx());
            widget(ui);
        })
        .drop_without_applying_deltas();
        asked
    }

    #[test]
    fn the_undo_shortcut_works_with_a_button_focused_and_not_in_a_text_field() {
        assert_eq!(
            ctrl_z_with_focus_on(|ui| ui.button("a button")),
            Some(Direction::Undo)
        );
        assert_eq!(
            ctrl_z_with_focus_on(|ui| ui.text_edit_singleline(&mut String::new())),
            None
        );
    }

    #[test]
    fn many_reasons_an_instrument_is_held_do_not_grow_the_bar_without_bound() {
        let held = Status::Held(
            (0..200)
                .map(|n| format!("lib/ui.json: fault {n}"))
                .collect(),
        );
        let ctx = egui::Context::default();
        let mut height = 0.0;
        // The top bar's nesting: a row offers its column one line of height, not the window's.
        ctx.run_ui(egui::RawInput::default(), |ui| {
            egui::Panel::top("bar").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        let top = ui.cursor().top();
                        status(ui, &held, "ui");
                        height = ui.cursor().top() - top;
                    });
                });
            });
        })
        .drop_without_applying_deltas();
        assert!(height > HELD_HEIGHT, "the reasons are drawn: {height}");
        assert!(height < HELD_HEIGHT + 40.0, "200 reasons took {height}");
    }
}
