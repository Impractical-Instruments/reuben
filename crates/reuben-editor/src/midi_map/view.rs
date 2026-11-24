//! Drawing the table. A text field's edit is asked for once, when it is committed: Enter or focus
//! leaving it, with something different in it.

use std::path::Path;

use eframe::egui;
use reuben_api::surface::SurfaceControl;

use super::model::{self, Action, MidiMapModel, Row, Status};
use crate::theme;

const PATTERN_WIDTH: f32 = 160.0;
const BOUND_WIDTH: f32 = 56.0;

/// What a row's fields hold while they are typed in.
#[derive(Debug, Clone, PartialEq)]
pub struct Draft {
    pub pattern: String,
    pub min: String,
    pub max: String,
}

impl Draft {
    pub fn of(row: &Row) -> Self {
        let bound = |b: Option<f64>| b.map(|b| b.to_string()).unwrap_or_default();
        Self {
            pattern: row.pattern.clone(),
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

/// Draw the map. `refused` is the last edit's refusal and the row it was of, `None` for an append.
pub fn table(
    ui: &mut egui::Ui,
    model: &MidiMapModel,
    drafts: &mut [Draft],
    refused: Option<&(Option<usize>, String)>,
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
            "{} does not exist yet: the first row creates it",
            file.to_string_lossy()
        ),
    });
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
        .num_columns(7)
        .show(ui, |ui| {
            for title in ["#", "Input", "Pattern", "Min", "Max", "Status", ""] {
                ui.strong(title);
            }
            ui.end_row();
            for (row, draft) in model.rows.iter().zip(drafts.iter_mut()) {
                let row_asked = ui
                    .push_id(row.index, |ui| {
                        self::row(ui, model, row, draft, refused_on(Some(row.index)))
                    })
                    .inner;
                asked = asked.take().or(row_asked);
                ui.end_row();
            }
        });
    if model.rows.is_empty() {
        ui.weak("No input is mapped.");
    }
    let add = model.added();
    let clicked = ui
        .add_enabled(add.is_some(), egui::Button::new("+ add"))
        .on_disabled_hover_text("The instrument has no interface input to map.")
        .clicked();
    if clicked {
        asked = asked.take().or(add.map(Asked::Edit));
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
) -> Option<Asked> {
    let at = row.index;
    let mut asked = None;
    let number = ui.label((at + 1).to_string());

    let mut bind = row.bind.clone();
    egui::ComboBox::from_id_salt("input")
        .selected_text(&bind)
        .show_ui(ui, |ui| {
            for input in &model.inputs {
                ui.selectable_value(&mut bind, input.clone(), input);
            }
        });
    if bind != row.bind {
        asked = set(model, at, |control| control.bind = bind);
    }

    let pattern = ui
        .add(egui::TextEdit::singleline(&mut draft.pattern).desired_width(PATTERN_WIDTH))
        .labelled_by(number.id);
    if pattern.lost_focus() && draft.pattern != row.pattern {
        let midi = draft.pattern.clone();
        asked = asked
            .take()
            .or(set(model, at, |control| control.midi = Some(midi)));
    }

    for (text, current, is_min) in [
        (&mut draft.min, row.min, true),
        (&mut draft.max, row.max, false),
    ] {
        let field = ui.add(egui::TextEdit::singleline(text).desired_width(BOUND_WIDTH));
        if !field.lost_focus() {
            continue;
        }
        let which = if is_min { "min" } else { "max" };
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
        status(ui, &row.status);
        if let Some(why) = refused {
            ui.colored_label(ui.visuals().error_fg_color, why);
        }
    });

    ui.horizontal(|ui| {
        let len = model.rows.len();
        let moved = [
            ("↑", "Move up", model::up(at)),
            ("↓", "Move down", model::down(at, len)),
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
        if ui.small_button("✕").on_hover_text("Remove").clicked() {
            asked = asked.take().or(Some(Asked::Edit(Action::Remove(at))));
        }
    });
    asked
}

/// The edit writing the control at `at` as the map holds it, changed by `change`.
fn set(model: &MidiMapModel, at: usize, change: impl FnOnce(&mut SurfaceControl)) -> Option<Asked> {
    let mut control = model.control(at)?.clone();
    change(&mut control);
    Some(Asked::Edit(Action::Set {
        index: Some(at),
        control,
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
