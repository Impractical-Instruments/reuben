//! Drawing the table. A text field's edit is asked for once, when it is committed: Enter or focus
//! leaving it, with something different in it.

use std::path::Path;

use eframe::egui;
use reuben_api::surface::{MidiKind, MidiSource, NumberPattern, SurfaceControl};

use super::model::{self, Action, MidiMapModel, Row, Status};
use crate::theme;

// Minimum widths, not desired ones: a Grid cell is only as wide as its column was last frame,
// which starts at the header's width, and a TextEdit or a wrapped label never grows past it.
// Wide enough for a channel or controller pattern such as `1-4,10`.
const NUMBERS_WIDTH: f32 = 72.0;
const BOUND_WIDTH: f32 = 56.0;
const STATUS_WIDTH: f32 = 240.0;

/// The row buttons' labels. Courier Prime draws none of the plain arrows and egui's Proportional
/// fallbacks draw neither them nor `✕`, so these are glyphs the editor's fonts do draw.
const UP: &str = "⬆";
const DOWN: &str = "⬇";
const REMOVE: &str = "×";

/// What a row's fields hold while they are typed in.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub channel: String,
    pub cc: String,
    pub min: String,
    pub max: String,
}

impl Draft {
    pub fn of(row: &Row) -> Self {
        let bound = |b: Option<f64>| b.map(|b| b.to_string()).unwrap_or_default();
        let midi = row.midi.as_ref();
        Self {
            channel: midi.map(|m| m.channel.to_string()).unwrap_or_default(),
            cc: (midi.and_then(|m| m.cc.as_ref()))
                .map(ToString::to_string)
                .unwrap_or_default(),
            min: bound(row.min),
            max: bound(row.max),
        }
    }
}

/// What the user asked of the table this frame.
pub enum Asked {
    Edit(Action),
    /// An edit that names no edit to make, on the row at the index, and why.
    Refuse(usize, String),
}

/// Draw the map. `refused` is the last edit's refusal and the row it was of, `None` for an append;
/// `created` is whether the last edit created the map.
pub fn table(
    ui: &mut egui::Ui,
    model: &MidiMapModel,
    drafts: &mut [Draft],
    refused: Option<&(Option<usize>, String)>,
    created: bool,
) -> Option<Asked> {
    theme::section(ui, "MIDI map");
    let Some(view) = &model.view else {
        match &model.error {
            Some(e) => ui.colored_label(ui.visuals().warn_fg_color, e),
            None => ui.weak("No instrument is open."),
        };
        return None;
    };
    let file = Path::new(&view.source).file_name().unwrap_or_default();
    ui.weak(match view.exists {
        true => file.to_string_lossy().into_owned(),
        false => format!(
            "{} does not exist yet: the first row creates it, which cannot be undone",
            file.to_string_lossy()
        ),
    });
    if created {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "The map was just created. Its creation cannot be undone: Undo steps back through the \
             edits made before it.",
        );
    }
    if let Some(e) = &model.error {
        ui.colored_label(
            ui.visuals().error_fg_color,
            format!("The MIDI map {e}. It is left as it is, and cannot be edited here."),
        );
        return None;
    }
    for warning in &view.warnings {
        ui.colored_label(ui.visuals().warn_fg_color, format!("warning: {warning}"));
    }
    let refused_on = |row: Option<usize>| {
        refused
            .filter(|(of, _)| *of == row)
            .map(|(_, why)| why.as_str())
    };
    let mut asked = None;
    egui::Grid::new("midi_map")
        .striped(true)
        .num_columns(9)
        .show(ui, |ui| {
            let mut cc_header = ui.id();
            for title in [
                "#", "Input", "Kind", "Channel", "CC", "Min", "Max", "Status", "",
            ] {
                let header = ui.strong(title);
                if title == "CC" {
                    cc_header = header.id;
                }
            }
            ui.end_row();
            for (row, draft) in model.rows.iter().zip(drafts.iter_mut()) {
                // Not under `push_id`: a Grid counts a child `Ui` as one cell, which would draw the
                // whole row in the first column.
                let refused = refused_on(Some(row.index));
                let row_asked = self::row(ui, model, row, draft, refused, cc_header);
                asked = asked.take().or(row_asked);
                ui.end_row();
            }
        });
    if model.rows.is_empty() {
        ui.weak("No input is mapped.");
    }
    match model.added() {
        Ok(add) => {
            if ui.button("+ add").clicked() {
                asked = asked.take().or(Some(Asked::Edit(add)));
            }
        }
        Err(why) => {
            ui.add_enabled(false, egui::Button::new("+ add"));
            ui.colored_label(ui.visuals().warn_fg_color, why);
        }
    }
    if let Some(why) = refused_on(None) {
        ui.colored_label(ui.visuals().error_fg_color, why);
    }
    asked
}

fn row(
    ui: &mut egui::Ui,
    model: &MidiMapModel,
    row: &Row,
    draft: &mut Draft,
    refused: Option<&str>,
    cc_header: egui::Id,
) -> Option<Asked> {
    let at = row.index;
    let mut asked = None;
    let number = ui.label((at + 1).to_string());

    let mut bind = row.bind.clone();
    egui::ComboBox::from_id_salt(("input", at))
        .selected_text(&bind)
        .show_ui(ui, |ui| {
            for input in &model.inputs {
                ui.selectable_value(&mut bind, input.clone(), input);
            }
        });
    if bind != row.bind {
        asked = model.rebound(at, &bind).map(Asked::Edit);
    }

    let kind = row.midi.as_ref().map(|midi| midi.kind);
    let mut picked = kind;
    egui::ComboBox::from_id_salt(("kind", at))
        .selected_text(kind.map_or("?", MidiKind::name))
        .show_ui(ui, |ui| {
            for offered in model.kinds(&row.bind) {
                ui.selectable_value(&mut picked, Some(offered), offered.name());
            }
        });
    if let Some(picked) = picked.filter(|picked| Some(*picked) != kind) {
        asked = asked.take().or(model.rekinded(at, picked).map(Asked::Edit));
    }

    let shown = Draft::of(row);
    let channel = ui
        .add(
            egui::TextEdit::singleline(&mut draft.channel)
                .id_salt(("channel", at))
                .min_size(egui::vec2(NUMBERS_WIDTH, 0.0)),
        )
        .labelled_by(number.id);
    if committed(ui, &channel, &mut draft.channel, &shown.channel) && draft.channel != shown.channel
    {
        let typed = NumberPattern::from_text(&draft.channel);
        asked = asked
            .take()
            .or(set_route(model, at, |midi| midi.channel = typed));
    }

    if kind == Some(MidiKind::Cc) {
        let cc = ui
            .add(
                egui::TextEdit::singleline(&mut draft.cc)
                    .id_salt(("cc", at))
                    .min_size(egui::vec2(NUMBERS_WIDTH, 0.0)),
            )
            .labelled_by(cc_header);
        if committed(ui, &cc, &mut draft.cc, &shown.cc) && draft.cc != shown.cc {
            let typed = NumberPattern::from_text(&draft.cc);
            asked = asked
                .take()
                .or(set_route(model, at, |midi| midi.cc = Some(typed)));
        }
    } else {
        // Only a `cc` route has a controller; the empty cell keeps the column's width.
        ui.add_enabled(
            false,
            egui::TextEdit::singleline(&mut String::new())
                .id_salt(("cc", at))
                .min_size(egui::vec2(NUMBERS_WIDTH, 0.0)),
        );
    }

    for (text, current, was, is_min) in [
        (&mut draft.min, row.min, &shown.min, true),
        (&mut draft.max, row.max, &shown.max, false),
    ] {
        let which = if is_min { "min" } else { "max" };
        let field = ui.add(
            egui::TextEdit::singleline(text)
                .id_salt((which, at))
                .min_size(egui::vec2(BOUND_WIDTH, 0.0)),
        );
        if !committed(ui, &field, text, was) {
            continue;
        }
        let bound = match text.trim() {
            "" => None,
            typed => match typed.parse::<f64>() {
                Ok(bound) => Some(bound),
                Err(_) => {
                    asked = asked.take().or(Some(Asked::Refuse(
                        at,
                        format!("{which} `{typed}` is not a number"),
                    )));
                    continue;
                }
            },
        };
        if bound != current {
            asked = asked.take().or(set(model, at, |control| match is_min {
                true => control.min = bound,
                false => control.max = bound,
            }));
        }
    }

    ui.vertical(|ui| {
        ui.set_min_width(STATUS_WIDTH);
        status(ui, &row.status);
        if let Some(why) = refused {
            ui.colored_label(ui.visuals().error_fg_color, why);
        }
    });

    ui.horizontal(|ui| {
        let len = model.rows.len();
        let moved = [
            (UP, "Move up", model::up(at)),
            (DOWN, "Move down", model::down(at, len)),
        ];
        for (glyph, hover, action) in moved {
            let clicked = ui
                .add_enabled(action.is_some(), egui::Button::new(glyph).small())
                .on_hover_text(hover)
                .clicked();
            if clicked {
                asked = asked.take().or(action.map(Asked::Edit));
            }
        }
        if ui.small_button(REMOVE).on_hover_text("Remove").clicked() {
            asked = asked.take().or(Some(Asked::Edit(Action::Remove(at))));
        }
    });
    asked
}

/// Whether the edit in `field` is committed this frame: Enter, or focus leaving it. Escape leaves
/// it too, but puts `draft` back to `shown`, what the map holds, and commits nothing.
fn committed(ui: &egui::Ui, field: &egui::Response, draft: &mut String, shown: &str) -> bool {
    if !field.lost_focus() {
        return false;
    }
    if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        *draft = shown.to_string();
        return false;
    }
    true
}

/// The edit writing the control at `at` with its route changed by `change`; a refusal for a row
/// that has no route yet, whose kind is to be picked first.
fn set_route(
    model: &MidiMapModel,
    at: usize,
    change: impl FnOnce(&mut MidiSource),
) -> Option<Asked> {
    let Some(midi) = model.control(at)?.midi.clone() else {
        return Some(Asked::Refuse(at, "pick a Kind first".to_string()));
    };
    set(model, at, |control| {
        let mut midi = midi;
        change(&mut midi);
        control.midi = Some(midi);
    })
}

/// The edit writing the control at `at` as the map holds it, changed by `change`.
fn set(model: &MidiMapModel, at: usize, change: impl FnOnce(&mut SurfaceControl)) -> Option<Asked> {
    let mut control = model.control(at)?.clone();
    change(&mut control);
    Some(Asked::Edit(Action::Set {
        index: Some(at),
        control: Box::new(control),
    }))
}

fn status(ui: &mut egui::Ui, status: &Status) {
    match status {
        Status::Ok => {
            ui.weak("ok");
        }
        Status::Warned(warnings) => {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                format!("warning: {}", warnings.join("; ")),
            );
        }
        Status::Skipped(why) => {
            ui.colored_label(ui.visuals().warn_fg_color, format!("skipped: {why}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_row_buttons_draw_with_the_editors_fonts() {
        for style in [egui::TextStyle::Button, egui::TextStyle::Body] {
            for glyph in [UP, DOWN, REMOVE] {
                assert!(
                    theme::draws(&style, glyph),
                    "no font of {style:?} draws {glyph}"
                );
            }
        }
    }
}
