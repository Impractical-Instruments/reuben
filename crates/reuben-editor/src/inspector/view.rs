//! Drawing the inspector, and turning what the pointer and keyboard do to its widgets into
//! [`Step`]s and [`Edit`]s.

use eframe::egui;
use reuben_api::authoring::{Diag, InstrumentStructure, StructureNode};
use reuben_api::engine::Channel;

use super::model::{drag_speed, fields, widget, Control, Field, Step, Typing, Widget};
use super::{interface, resources, Edit, Inspector};
use crate::canvas::Item;
use crate::feedback::Feedback;
use crate::session::Session;

const SLIDER_WIDTH: f32 = 180.0;

/// What one frame of the inspector asks for, in the order it was asked.
pub(super) enum Ask {
    Step(Step),
    /// An edit, and the content hash it was made against, or `None` for the document on screen.
    Edit(Edit, Option<String>),
    /// An edit asked for that names no edit to make, and why: it is reported as the verb's own
    /// refusal would be.
    Refuse(String),
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
                    let rejected = (session.feedback()).map_or(&[][..], |feedback| {
                        feedback.rejected(&session.viewed().source)
                    });
                    let problems = problems(top, rejected, selection);
                    inspector.holds.begin();
                    inspector.drafts.begin();
                    match selection {
                        None => {
                            instrument(ui, inspector, top, &problems, &mut asks);
                            interface::list(ui, inspector, top, &mut asks);
                            let document = &session.viewed().path;
                            resources::list(ui, inspector, top, document, &mut asks);
                        }
                        Some(Item::Node(address)) => {
                            match top.nodes.iter().find(|n| &n.address == address) {
                                Some(node) => {
                                    self::node(
                                        ui, inspector, node, &top.hash, &problems, &mut asks,
                                    );
                                    resources::slots(ui, node, top, &mut asks);
                                }
                                None => {
                                    ui.weak(format!("{address} is not in the document."));
                                }
                            }
                        }
                        Some(Item::Input(name)) => {
                            interface::input(ui, inspector, top, name, &problems, &mut asks)
                        }
                        Some(Item::Output(name)) => {
                            interface::output(ui, inspector, top, name, &problems, &mut asks)
                        }
                    }
                    asks.extend(inspector.holds.end().map(Ask::Step));
                    inspector.drafts.end();
                });
        });
    for ask in asks {
        match ask {
            Ask::Step(step) => inspector.step(step, session, channel),
            Ask::Edit(edit, expect) => {
                inspector.commit(edit, expect, session, channel);
            }
            Ask::Refuse(message) => refuse(session, message),
        }
    }
}

/// One diagnostic about what the inspector shows, and whether it is an error.
pub(super) type Problem<'a> = (bool, &'a Diag);

/// What the last rejected edit of `top`, then its load report, say about `selection`: a node's
/// own, a pipe's own, or with nothing selected those that name neither. A load error the
/// rejection repeats is listed once.
fn problems<'a>(
    top: &'a InstrumentStructure,
    rejected: &'a [Diag],
    selection: Option<&Item>,
) -> Vec<Problem<'a>> {
    let about = |diag: &&Diag| match selection {
        None => diag.node.is_none() && diag.port.is_none(),
        Some(Item::Node(address)) => diag.node.as_ref() == Some(address),
        Some(Item::Input(name) | Item::Output(name)) => {
            diag.node.is_none() && diag.port.as_ref() == Some(name)
        }
    };
    let rejected = rejected.iter().filter(about).map(|d| (true, d));
    let loaded = (top.report.errors.iter().map(|d| (true, d)))
        .chain(top.report.warnings.iter().map(|d| (false, d)))
        .filter(|(_, d)| about(d) && !rejected.clone().any(|(_, r)| r == *d));
    rejected.clone().chain(loaded).collect()
}

/// `problems`, each in its colour and led by the node port it names.
pub(super) fn listed(ui: &mut egui::Ui, problems: &[Problem<'_>]) {
    for (error, diag) in problems {
        let color = if *error {
            ui.visuals().error_fg_color
        } else {
            ui.visuals().warn_fg_color
        };
        let text = match (&diag.node, &diag.port) {
            (Some(_), Some(port)) => format!("{port}: {}", diag.message),
            _ => diag.message.clone(),
        };
        ui.colored_label(color, text);
    }
}

fn instrument(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    top: &InstrumentStructure,
    problems: &[Problem<'_>],
    asks: &mut Vec<Ask>,
) {
    ui.weak("Instrument");
    listed(ui, problems);
    let current = Current {
        text: &top.instrument,
        hash: &top.hash,
    };
    if let Some((name, hash)) = text(ui, inspector, "Name", ("", "name"), current, false) {
        asks.push(Ask::Edit(Edit::Name(name), Some(hash)));
    }
    let current = Current {
        text: top.description.as_deref().unwrap_or(""),
        hash: &top.hash,
    };
    let field = ("", "description");
    if let Some((text, hash)) = text(ui, inspector, "Description", field, current, true) {
        asks.push(Ask::Edit(Edit::Description(text), Some(hash)));
    }
}

fn node(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    node: &StructureNode,
    hash: &str,
    problems: &[Problem<'_>],
    asks: &mut Vec<Ask>,
) {
    ui.heading(&node.address);
    ui.weak(&node.type_name);
    listed(ui, problems);
    let current = Current {
        text: node.description.as_deref().unwrap_or(""),
        hash,
    };
    let field = (node.address.as_str(), "description");
    if let Some((text, hash)) = text(ui, inspector, "Description", field, current, true) {
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
                    row(ui, inspector, field, hash, problems, asks);
                    if let Some(edit) = reset(ui, field) {
                        asks.push(Ask::Edit(edit, Some(hash.to_string())));
                    }
                    ui.end_row();
                }
            });
    }
}

/// A button on a constant the node sets, to remove it so the operator's default applies again.
fn reset(ui: &mut egui::Ui, field: &Field) -> Option<Edit> {
    let edit = removal(field)?;
    (ui.small_button("reset"))
        .on_hover_text("Remove this constant: the operator's default applies.")
        .clicked()
        .then_some(edit)
}

/// The edit removing `field`'s constant, when it is a constant the node sets. A node of unknown
/// type has no default to fall back to, so its constants have none.
pub(super) fn removal(field: &Field) -> Option<Edit> {
    (field.target.constant && field.set && !field.kind.is_empty()).then(|| Edit::RemoveConstant {
        address: field.target.address.clone(),
        name: field.target.name.clone(),
    })
}

fn row(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    field: &Field,
    hash: &str,
    problems: &[Problem<'_>],
    asks: &mut Vec<Ask>,
) {
    labelled_row(
        ui,
        inspector,
        &field.target.name,
        field,
        hash,
        problems,
        asks,
    );
}

/// [`row`], with `label` in place of the name of the input it sets.
pub(super) fn labelled_row(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    label: &str,
    field: &Field,
    hash: &str,
    problems: &[Problem<'_>],
    asks: &mut Vec<Ask>,
) {
    let named: Vec<&Problem<'_>> = (problems.iter())
        .filter(|(_, d)| d.port.as_ref() == Some(&field.target.name))
        .collect();
    let name = if named.iter().any(|(error, _)| *error) {
        ui.colored_label(ui.visuals().error_fg_color, label)
    } else if !named.is_empty() {
        ui.colored_label(ui.visuals().warn_fg_color, label)
    } else {
        ui.label(label)
    };
    let mut hint = if field.set {
        field.kind.clone()
    } else {
        format!("{} — the default", field.kind)
    };
    for (_, diag) in named {
        hint.push('\n');
        hint.push_str(&diag.message);
    }
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
pub(super) struct Current<'a> {
    pub text: &'a str,
    pub hash: &'a str,
}

/// `label`, then a text box for `field` of `owner` named by it, showing the document's text until it
/// is typed into. Returns the new text, and the hash to write it against, when the box loses focus
/// with something different in it.
pub(super) fn text(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    label: &str,
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
    let label = ui.label(label);
    let response = ui
        .add(edit.desired_width(f32::INFINITY))
        .labelled_by(label.id);
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

/// Hold `message` as the session's feedback, as the refusal of an edit of the viewed document.
pub(super) fn refuse(session: &mut Session, message: String) {
    let source = session.viewed().source.clone();
    *session.feedback_mut() = Some(Feedback::Refused { source, message });
}

#[cfg(test)]
mod tests {
    use reuben_api::authoring::{read_instrument, ReadInstrument};
    use reuben_api::FsResolver;
    use serde_json::json;

    use super::*;

    fn diag(node: Option<&str>, port: Option<&str>, message: &str) -> Diag {
        Diag {
            node: node.map(str::to_string),
            port: port.map(str::to_string),
            message: message.to_string(),
        }
    }

    #[test]
    fn the_selection_shows_what_the_rejection_and_the_load_report_say_about_it() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../instruments/default.json"
        );
        let mut top = read_instrument(
            &ReadInstrument {
                source: path.to_string(),
            },
            &FsResolver::for_document(path).stat_only(),
        )
        .expect("it reads")
        .output;
        top.report = serde_json::from_value(json!({
            "ok": true,
            "errors": [],
            "warnings": [
                { "node": "/out", "message": "careful" },
                { "port": "notes", "message": "unbound" },
                { "message": "everywhere" },
            ],
        }))
        .unwrap();
        let rejected = [
            diag(Some("/out"), Some("audio"), "refused"),
            diag(Some("/voicer"), None, "elsewhere"),
        ];
        let said = |selection: Option<Item>| -> Vec<(bool, String)> {
            problems(&top, &rejected, selection.as_ref())
                .into_iter()
                .map(|(error, d)| (error, d.message.clone()))
                .collect()
        };
        assert_eq!(
            said(Some(Item::Node("/out".into()))),
            [(true, "refused".into()), (false, "careful".into())]
        );
        assert_eq!(
            said(Some(Item::Input("notes".into()))),
            [(false, "unbound".into())]
        );
        assert_eq!(said(None), [(false, "everywhere".into())]);
    }
}
