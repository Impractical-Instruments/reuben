//! Drawing the inspector, and turning what the pointer and keyboard do to its widgets into
//! [`Step`]s and [`Edit`]s.

use eframe::egui;
use reuben_api::authoring::{InstrumentStructure, StructureNode};
use reuben_api::engine::Channel;

use super::model::{drag_speed, fields, widget, Control, Field, Step, Typing, Widget};
use super::{Edit, Inspector};
use crate::canvas::Item;
use crate::session::Session;

const SLIDER_WIDTH: f32 = 180.0;

/// What one frame of the inspector asks for, in the order it was asked.
enum Ask {
    Step(Step),
    /// An edit, and the content hash it was made against, or `None` for the document on screen.
    Edit(Edit, Option<String>),
}

/// Draw the inspector down the right of `ui` for `selection` in the document the session views,
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
                    let top = match &session.viewed().read {
                        Ok(top) => top,
                        Err(e) => {
                            inspector.unreadable();
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
                    inspector.drafts.begin();
                    match selection {
                        None => instrument(ui, inspector, top, &mut asks),
                        Some(Item::Node(address)) => {
                            match top.nodes.iter().find(|n| &n.address == address) {
                                Some(node) => self::node(ui, inspector, node, &top.hash, &mut asks),
                                None => {
                                    ui.weak(format!("{address} is not in the document."));
                                }
                            }
                        }
                        Some(Item::Input(name)) => pipe(ui, "Interface input", name),
                        Some(Item::Output(name)) => pipe(ui, "Interface output", name),
                    }
                    asks.extend(inspector.holds.end().map(Ask::Step));
                    inspector.drafts.end();
                });
        });
    for ask in asks {
        match ask {
            Ask::Step(step) => inspector.step(step, session, channel),
            Ask::Edit(edit, expect) => inspector.commit(edit, expect, session, channel),
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
    let current = Current {
        text: &top.instrument,
        hash: &top.hash,
    };
    if let Some((name, hash)) = text(ui, inspector, ("", "name"), current, false) {
        asks.push(Ask::Edit(Edit::Name(name), Some(hash)));
    }
    ui.label("Description");
    let current = Current {
        text: top.description.as_deref().unwrap_or(""),
        hash: &top.hash,
    };
    if let Some((text, hash)) = text(ui, inspector, ("", "description"), current, true) {
        asks.push(Ask::Edit(Edit::Description(text), Some(hash)));
    }
}

fn node(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    node: &StructureNode,
    hash: &str,
    asks: &mut Vec<Ask>,
) {
    ui.heading(&node.address);
    ui.weak(&node.type_name);
    ui.label("Description");
    let current = Current {
        text: node.description.as_deref().unwrap_or(""),
        hash,
    };
    if let Some((text, hash)) = text(ui, inspector, (&node.address, "description"), current, true) {
        asks.push(Ask::Edit(
            Edit::NodeDescription {
                address: node.address.clone(),
                text,
            },
            Some(hash),
        ));
    }

    let fields = fields(node);
    let (constants, inputs): (Vec<&Field>, Vec<&Field>) =
        fields.iter().partition(|f| f.target.constant);
    for (title, fields) in [("Inputs", inputs), ("Constants", constants)] {
        if fields.is_empty() {
            continue;
        }
        ui.separator();
        crate::theme::section(ui, title);
        egui::Grid::new(title)
            .num_columns(2)
            .striped(true)
            .show(ui, |ui| {
                for field in fields {
                    row(ui, inspector, field, hash, asks);
                    ui.end_row();
                }
            });
    }
}

fn row(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    field: &Field,
    hash: &str,
    asks: &mut Vec<Ask>,
) {
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
            let steps =
                inspector
                    .holds
                    .number(target, (*value, hash), shown, holding, response.changed());
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
                asks.push(Ask::Edit(
                    Edit::Value {
                        target: field.target.clone(),
                        value: serde_json::Value::String(picked),
                    },
                    None,
                ));
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
    match widget(min, max, exponential) {
        Widget::Slider {
            min,
            max,
            logarithmic,
        } => {
            ui.spacing_mut().slider_width = SLIDER_WIDTH;
            let mut slider = egui::Slider::new(value, min..=max)
                .logarithmic(logarithmic)
                // A value the document holds outside the range is shown as it is, not clamped
                // into it: clamping on draw would be a change no one made.
                .clamping(egui::SliderClamping::Edits)
                .show_value(false);
            if integer {
                slider = slider.integer();
            }
            let slid = ui.add(slider);
            slid | ui.add(typed(value, &suffix, integer).range(min..=max))
        }
        Widget::Drag { min, max } => {
            let speed = drag_speed(*value, integer);
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
        .update_while_editing(false)
        .clamp_existing_to_range(false);
    if integer {
        drag.fixed_decimals(0)
    } else {
        drag
    }
}

/// What the document holds for a text box, and the document's content hash.
#[derive(Clone, Copy)]
struct Current<'a> {
    text: &'a str,
    hash: &'a str,
}

/// A text box for `field` of `owner`, showing the document's text until it is typed into. Returns
/// the new text, and the hash to write it against, when the box loses focus with something
/// different in it.
fn text(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    (owner, field): (&str, &'static str),
    current: Current<'_>,
    multiline: bool,
) -> Option<(String, String)> {
    let mut text = (inspector.drafts.text(owner, field))
        .unwrap_or(current.text)
        .to_string();
    let edit = if multiline {
        egui::TextEdit::multiline(&mut text).desired_rows(3)
    } else {
        egui::TextEdit::singleline(&mut text)
    };
    let response = ui.add(edit.desired_width(f32::INFINITY));
    let typing = Typing {
        text,
        focused: response.has_focus(),
        lost: response.lost_focus(),
        escaped: ui.input(|i| i.key_pressed(egui::Key::Escape)),
    };
    inspector
        .drafts
        .frame(owner, field, current.text, current.hash, typing)
}
