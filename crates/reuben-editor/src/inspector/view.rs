//! Drawing the inspector, and turning what the pointer and keyboard do to its widgets into
//! [`Step`]s and [`Edit`]s.

use eframe::egui;
use reuben_api::authoring::{InstrumentStructure, StructureNode};
use reuben_api::engine::Channel;

use super::model::{fields, Control, Field, Step};
use super::{Draft, Edit, Inspector};
use crate::canvas::Item;
use crate::session::Session;

const SLIDER_WIDTH: f32 = 180.0;

/// What one frame of the inspector asks for, in the order it was asked.
enum Ask {
    Step(Step),
    Edit(Edit),
}

/// Draw the inspector down the right of `ui` for `selection` in the session's top-level document,
/// and carry out what was done to it.
pub fn show(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    session: &mut Session,
    channel: &dyn Fn() -> Option<Channel>,
    selection: Option<&Item>,
) {
    let mut asks = Vec::new();
    egui::Panel::right("inspector")
        .resizable(true)
        .default_size(320.0)
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    let top = match &session.tree().top().read {
                        Ok(top) => top,
                        Err(e) => {
                            ui.colored_label(
                                ui.visuals().error_fg_color,
                                format!(
                                    "Nothing to inspect: the instrument could not be read. {e}"
                                ),
                            );
                            return;
                        }
                    };
                    if let Some(e) = &inspector.error {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                        ui.separator();
                    }
                    inspector.holds.begin();
                    match selection {
                        None => instrument(ui, inspector, top, &mut asks),
                        Some(Item::Node(address)) => {
                            match top.nodes.iter().find(|n| &n.address == address) {
                                Some(node) => self::node(ui, inspector, node, &mut asks),
                                None => {
                                    ui.weak(format!("{address} is not in the document."));
                                }
                            }
                        }
                        Some(Item::Input(name)) => pipe(ui, "Interface input", name),
                        Some(Item::Output(name)) => pipe(ui, "Interface output", name),
                    }
                    asks.extend(inspector.holds.end().map(Ask::Step));
                });
        });
    for ask in asks {
        match ask {
            Ask::Step(step) => inspector.step(step, session, channel),
            Ask::Edit(edit) => inspector.commit(edit, session, channel),
        }
    }
}

fn pipe(ui: &mut egui::Ui, what: &str, name: &str) {
    ui.heading(name);
    ui.weak(what);
}

fn instrument(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    top: &InstrumentStructure,
    asks: &mut Vec<Ask>,
) {
    ui.weak("Instrument");
    ui.label("Name");
    if let Some(name) = text(ui, inspector, "", "name", &top.instrument, false) {
        asks.push(Ask::Edit(Edit::Name(name)));
    }
    ui.label("Description");
    let description = top.description.as_deref().unwrap_or("");
    if let Some(text) = text(ui, inspector, "", "description", description, true) {
        asks.push(Ask::Edit(Edit::Description(text)));
    }
}

fn node(ui: &mut egui::Ui, inspector: &mut Inspector, node: &StructureNode, asks: &mut Vec<Ask>) {
    ui.heading(&node.address);
    ui.weak(&node.type_name);
    ui.label("Description");
    let description = node.description.as_deref().unwrap_or("");
    if let Some(text) = text(
        ui,
        inspector,
        &node.address,
        "description",
        description,
        true,
    ) {
        asks.push(Ask::Edit(Edit::NodeDescription {
            address: node.address.clone(),
            text,
        }));
    }

    let fields = fields(node);
    let (constants, inputs): (Vec<&Field>, Vec<&Field>) =
        fields.iter().partition(|f| f.target.constant);
    for (title, fields) in [("Inputs", inputs), ("Constants", constants)] {
        if fields.is_empty() {
            continue;
        }
        ui.separator();
        ui.strong(title);
        egui::Grid::new(title)
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for field in fields {
                    row(ui, inspector, field, asks);
                    ui.end_row();
                }
            });
    }
}

fn row(ui: &mut egui::Ui, inspector: &mut Inspector, field: &Field, asks: &mut Vec<Ask>) {
    let name = ui.label(&field.target.name);
    let hint = if field.set {
        field.kind.clone()
    } else {
        format!("{} — the default", field.kind)
    };
    name.on_hover_text(hint);
    match &field.control {
        Control::Number {
            value,
            min,
            max,
            exponential,
            unit,
        } => {
            let target = &field.target;
            let mut shown = inspector.holds.value(target).unwrap_or(*value);
            let response = ui
                .horizontal(|ui| {
                    number(
                        ui,
                        &mut shown,
                        *min,
                        *max,
                        *exponential,
                        unit,
                        target.integer,
                    )
                })
                .inner;
            let holding = response.is_pointer_button_down_on() || response.dragged();
            let steps = inspector
                .holds
                .number(target, *value, shown, holding, response.changed());
            asks.extend(steps.into_iter().map(Ask::Step));
        }
        Control::Choice { variants, value } => {
            let mut picked = value.clone();
            egui::ComboBox::from_id_salt(("choice", &field.target.address, &field.target.name))
                .selected_text(&picked)
                .show_ui(ui, |ui| {
                    for variant in variants {
                        ui.selectable_value(&mut picked, variant.clone(), variant);
                    }
                });
            if &picked != value {
                asks.push(Ask::Edit(Edit::Value {
                    target: field.target.clone(),
                    value: serde_json::Value::String(picked),
                }));
            }
        }
        Control::Wired { from } => {
            ui.weak(format!("← {from}"));
        }
        Control::Shown { text } => {
            ui.weak(text);
        }
    }
}

/// A slider across a declared range, or a drag value without one, plus a box to type a value
/// into. The response is the slider's or drag value's, joined with the box's.
fn number(
    ui: &mut egui::Ui,
    value: &mut f64,
    min: Option<f64>,
    max: Option<f64>,
    exponential: bool,
    unit: &str,
    integer: bool,
) -> egui::Response {
    let suffix = if unit.is_empty() {
        String::new()
    } else {
        format!(" {unit}")
    };
    match (min, max) {
        (Some(min), Some(max)) if min < max => {
            ui.spacing_mut().slider_width = SLIDER_WIDTH;
            let mut slider = egui::Slider::new(value, min..=max)
                // A logarithmic slider cannot start at zero.
                .logarithmic(exponential && min > 0.0)
                .show_value(false);
            if integer {
                slider = slider.integer();
            }
            let slid = ui.add(slider);
            slid | ui.add(typed(value, &suffix, integer).range(min..=max))
        }
        (min, max) => {
            let speed = if integer { 0.1 } else { 0.01 };
            let drag = typed(value, &suffix, integer)
                .speed(speed)
                .range(min.unwrap_or(f64::NEG_INFINITY)..=max.unwrap_or(f64::INFINITY));
            ui.add(drag)
        }
    }
}

fn typed<'a>(value: &'a mut f64, suffix: &str, integer: bool) -> egui::DragValue<'a> {
    let drag = egui::DragValue::new(value)
        .suffix(suffix)
        // A typed value lands once, on Enter or when the box loses focus, not per keystroke.
        .update_while_editing(false);
    if integer {
        drag.fixed_decimals(0)
    } else {
        drag
    }
}

/// A text box for one of `owner`'s text fields, showing `current` until it is typed into. Returns
/// the new text when the box loses focus with something different in it.
fn text(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    owner: &str,
    field: &'static str,
    current: &str,
    multiline: bool,
) -> Option<String> {
    let mine = |d: &Draft| d.owner == owner && d.field == field;
    let mut text = match &inspector.draft {
        Some(draft) if mine(draft) => draft.text.clone(),
        _ => current.to_string(),
    };
    let edit = if multiline {
        egui::TextEdit::multiline(&mut text).desired_rows(3)
    } else {
        egui::TextEdit::singleline(&mut text)
    };
    let response = ui.add(edit.desired_width(f32::INFINITY));
    if response.has_focus() {
        inspector.draft = Some(Draft {
            owner: owner.to_string(),
            field,
            text,
        });
        return None;
    }
    if response.lost_focus() || inspector.draft.as_ref().is_some_and(mine) {
        inspector.draft = None;
        if text != current {
            return Some(text);
        }
    }
    None
}
