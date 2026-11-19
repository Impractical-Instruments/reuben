//! The instrument's interface in the inspector: its input and output pipes, added and removed with
//! nothing selected, and the metadata of the pipe selected on the canvas.
//!
//! Each change is one authoring verb. A pipe's type is chosen when it is added: no verb changes it.
//! A metadata field can be set and not cleared, as `set_instrument_interface_input_meta` and
//! `set_instrument_interface_output_meta` write only the fields they are given.

use eframe::egui;
use reuben_api::authoring::{
    add_instrument_interface_input, add_instrument_interface_output,
    remove_instrument_interface_input, remove_instrument_interface_output,
    set_instrument_interface_input_meta, set_instrument_interface_output_meta,
    wire_instrument_interface_output, AddInstrumentInterfaceInput, AddInstrumentInterfaceOutput,
    Answer, EditResult, InstrumentStructure, InterfaceInput, InterfaceOutput, Refusal,
    RemoveInstrumentInterfacePipe, Resources, SetInstrumentInterfaceInputMeta,
    SetInstrumentInterfaceOutputMeta, WireInstrumentInterfaceOutput,
};
use serde_json::Value;

use super::model::{pipe_field, wire_source};
use super::view::{labelled_row, listed, text, Ask, Current, Problem};
use super::{Edit, Inspector};

/// The types an interface input pipe can be declared as, besides an enum type's name.
const PIPE_TYPES: [&str; 6] = ["f32", "f32_buffer", "i32", "note", "harmony", "pitch"];

/// One change to the interface.
#[derive(Debug, Clone, PartialEq)]
pub enum PipeEdit {
    AddInput {
        name: String,
        type_name: String,
        value: Option<Value>,
    },
    AddOutput {
        name: String,
        from: String,
    },
    RemoveInput(String),
    RemoveOutput(String),
    InputMeta {
        name: String,
        meta: Meta,
    },
    OutputMeta {
        name: String,
        meta: Meta,
    },
    /// The output pipe `name`, fed from the wire-ref `from` in place of what fed it.
    Feed {
        name: String,
        from: String,
    },
}

/// Metadata to write on a pipe. A field left `None` is left as the document has it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Meta {
    pub min: Option<f64>,
    pub max: Option<f64>,
    /// `lin` or `exp`. Input pipes only.
    pub curve: Option<String>,
    pub unit: Option<String>,
    pub channel: Option<usize>,
}

impl PipeEdit {
    pub fn run(
        self,
        source: String,
        store: &dyn Resources,
        expect: Option<String>,
    ) -> Result<Answer<EditResult>, Refusal> {
        match self {
            PipeEdit::AddInput {
                name,
                type_name,
                value,
            } => add_instrument_interface_input(
                &AddInstrumentInterfaceInput {
                    source,
                    name,
                    type_name,
                    value,
                    expect,
                    ..Default::default()
                },
                store,
            ),
            PipeEdit::AddOutput { name, from } => add_instrument_interface_output(
                &AddInstrumentInterfaceOutput {
                    source,
                    name,
                    from,
                    expect,
                    ..Default::default()
                },
                store,
            ),
            PipeEdit::RemoveInput(name) => remove_instrument_interface_input(
                &RemoveInstrumentInterfacePipe {
                    source,
                    name,
                    expect,
                },
                store,
            ),
            PipeEdit::RemoveOutput(name) => remove_instrument_interface_output(
                &RemoveInstrumentInterfacePipe {
                    source,
                    name,
                    expect,
                },
                store,
            ),
            PipeEdit::InputMeta { name, meta } => set_instrument_interface_input_meta(
                &SetInstrumentInterfaceInputMeta {
                    source,
                    name,
                    channel: meta.channel,
                    min: meta.min,
                    max: meta.max,
                    curve: meta.curve,
                    unit: meta.unit,
                    expect,
                },
                store,
            ),
            PipeEdit::OutputMeta { name, meta } => set_instrument_interface_output_meta(
                &SetInstrumentInterfaceOutputMeta {
                    source,
                    name,
                    channel: meta.channel,
                    min: meta.min,
                    max: meta.max,
                    unit: meta.unit,
                    expect,
                },
                store,
            ),
            PipeEdit::Feed { name, from } => wire_instrument_interface_output(
                &WireInstrumentInterfaceOutput {
                    source,
                    name,
                    from,
                    expect,
                },
                store,
            ),
        }
    }
}

/// What the "add a pipe" rows hold between frames.
#[derive(Debug, Clone, PartialEq)]
pub struct Adding {
    input_name: String,
    input_type: String,
    input_value: String,
    output_name: String,
    /// The wire-ref picked to feed the new output; `None` takes [`default_feed`].
    output_from: Option<String>,
}

impl Default for Adding {
    fn default() -> Self {
        Self {
            input_name: String::new(),
            input_type: PIPE_TYPES[0].to_string(),
            input_value: String::new(),
            output_name: String::new(),
            output_from: None,
        }
    }
}

impl Adding {
    /// The new input pipe the row describes, or why it is not one yet.
    pub fn input(&self) -> Result<PipeEdit, String> {
        let name = self.input_name.trim();
        if name.is_empty() {
            return Err("A new interface input needs a name.".to_string());
        }
        let value = self.input_value.trim();
        let value = match value.parse::<f64>() {
            _ if value.is_empty() => None,
            Ok(number) => Some(serde_json::json!(number)),
            // An enum pipe's value is one of its type's symbols.
            Err(_) => Some(Value::String(value.to_string())),
        };
        Ok(PipeEdit::AddInput {
            name: name.trim_start_matches('/').to_string(),
            type_name: self.input_type.trim().to_string(),
            value,
        })
    }

    /// The new output pipe the row describes over `top`, or why it is not one yet.
    pub fn output(&self, top: &InstrumentStructure) -> Result<PipeEdit, String> {
        let from = (self.output_from.clone())
            .or_else(|| default_feed(top))
            .ok_or("An interface output is fed from a node's output: add a node first.")?;
        let name = match self.output_name.trim() {
            "" => free_output_name(top),
            typed => typed.to_string(),
        };
        Ok(PipeEdit::AddOutput { name, from })
    }

    pub fn added(&mut self, edit: &PipeEdit) {
        match edit {
            PipeEdit::AddInput { .. } => {
                self.input_name.clear();
                self.input_value.clear();
            }
            PipeEdit::AddOutput { .. } => {
                self.output_name.clear();
                self.output_from = None;
            }
            _ => {}
        }
    }
}

/// Every output an output pipe can be fed from, as wire-refs: the interface inputs, passed
/// straight through, then each node's outputs, in document order.
fn feeds(top: &InstrumentStructure) -> Vec<String> {
    let pipes = (top.inputs.iter()).map(|pipe| format!("/{}", pipe.port.name));
    let nodes = (top.nodes.iter()).flat_map(|node| {
        (node.ports.outputs.iter()).map(|port| format!("{}.{}", node.address, port.name))
    });
    pipes.chain(nodes).collect()
}

/// The output a new interface output is fed from unless another is picked: the last node's first
/// signal output, a chain usually ending in what is heard, else the first output of any kind.
fn default_feed(top: &InstrumentStructure) -> Option<String> {
    let signal = (top.nodes.iter().rev()).find_map(|node| {
        (node.ports.outputs.iter())
            .find(|port| port.kind == "signal")
            .map(|port| format!("{}.{}", node.address, port.name))
    });
    let first = (top.nodes.iter()).find_map(|node| {
        (node.ports.outputs.first()).map(|p| format!("{}.{}", node.address, p.name))
    });
    signal.or(first)
}

/// `out`, else the first free `out_2`, `out_3`, …: the names a document's outputs are given when
/// nothing names them.
fn free_output_name(top: &InstrumentStructure) -> String {
    let taken = |name: &str| top.outputs.iter().any(|o| o.name == name);
    std::iter::once("out".to_string())
        .chain((2..).map(|n| format!("out_{n}")))
        .find(|name| !taken(name))
        .expect("an unbounded range has a free name")
}

/// The interface pipes listed under the instrument, with a row to add each kind.
pub fn list(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    top: &InstrumentStructure,
    asks: &mut Vec<Ask>,
) {
    ui.separator();
    crate::theme::section(ui, "Interface inputs");
    for pipe in &top.inputs {
        ui.horizontal(|ui| {
            ui.label(&pipe.port.name);
            ui.weak(&pipe.port.kind);
        });
    }
    let adding = &mut inspector.adding;
    let mut add = false;
    egui::Grid::new("add input").num_columns(2).show(ui, |ui| {
        ui.label("Name");
        add |=
            entered(ui.add(egui::TextEdit::singleline(&mut adding.input_name).hint_text("cutoff")));
        ui.end_row();
        ui.label("Type");
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut adding.input_type).desired_width(90.0))
                .on_hover_text("f32, f32_buffer, i32, note, harmony, pitch, or an enum type name");
            egui::ComboBox::from_id_salt("pipe types")
                .selected_text("")
                .width(20.0)
                .show_ui(ui, |ui| {
                    for ty in PIPE_TYPES {
                        ui.selectable_value(&mut adding.input_type, ty.to_string(), ty);
                    }
                });
        });
        ui.end_row();
        ui.label("Value");
        add |= entered(
            ui.add(egui::TextEdit::singleline(&mut adding.input_value).hint_text("optional")),
        );
        ui.end_row();
    });
    if ui.button("Add input").clicked() || add {
        asks.push(ask(adding.input()));
    }

    ui.separator();
    crate::theme::section(ui, "Interface outputs");
    for pipe in &top.outputs {
        ui.horizontal(|ui| {
            ui.label(&pipe.name);
            ui.weak(format!("← {}", wire_source(&pipe.from)));
        });
    }
    let adding = &mut inspector.adding;
    let feeds = feeds(top);
    // A pick the document no longer has, its node removed or renamed, is let go of.
    if (adding.output_from.as_ref()).is_some_and(|from| !feeds.contains(from)) {
        adding.output_from = None;
    }
    let mut add = false;
    egui::Grid::new("add output").num_columns(2).show(ui, |ui| {
        ui.label("Name");
        let hint = free_output_name(top);
        add |= entered(ui.add(egui::TextEdit::singleline(&mut adding.output_name).hint_text(hint)));
        ui.end_row();
        ui.label("Fed from");
        let shown = (adding.output_from.clone())
            .or_else(|| default_feed(top))
            .unwrap_or_else(|| "no node has an output".to_string());
        egui::ComboBox::from_id_salt("new output feed")
            .selected_text(shown)
            .show_ui(ui, |ui| {
                for feed in &feeds {
                    ui.selectable_value(&mut adding.output_from, Some(feed.clone()), feed);
                }
            });
        ui.end_row();
    });
    if ui.button("Add output").clicked() || add {
        asks.push(ask(adding.output(top)));
    }
}

/// The ask an add button makes: its edit, or why the row names none yet.
fn ask(edit: Result<PipeEdit, String>) -> Ask {
    match edit {
        Ok(edit) => Ask::Edit(Edit::Pipe(edit), None),
        Err(why) => Ask::Refuse(why),
    }
}

/// Enter pressed in a single-line box: it gives up focus as it does.
fn entered(response: egui::Response) -> bool {
    response.lost_focus() && response.ctx.input(|i| i.key_pressed(egui::Key::Enter))
}

/// The interface input pipe `name` of `top`, selected on the canvas.
pub fn input(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    top: &InstrumentStructure,
    name: &str,
    problems: &[Problem<'_>],
    asks: &mut Vec<Ask>,
) {
    let Some(pipe) = top.inputs.iter().find(|p| p.port.name == name) else {
        ui.weak(format!("No interface input {name} in the document."));
        return;
    };
    let InterfaceInput { port, .. } = pipe;
    ui.heading(name);
    ui.weak(format!("Interface input · {}", port.kind));
    listed(ui, problems);
    ui.separator();
    let owner = format!("<input>{name}");
    let meta = egui::Grid::new("input pipe")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            let mut meta = None;
            labelled_row(
                ui,
                inspector,
                "value",
                &pipe_field(port),
                &top.hash,
                &[],
                asks,
            );
            ui.end_row();
            let current = |text| Current {
                text,
                hash: &top.hash,
            };
            // The loader takes a range on a number pipe only, and a curve on a continuous one.
            let numeric = matches!(port.kind.as_str(), "value" | "signal" | "int");
            let curved = matches!(port.kind.as_str(), "value" | "signal");
            let min = number_text(port.min);
            let max = number_text(port.max);
            if numeric {
                meta = meta.or(field(
                    ui,
                    inspector,
                    (&owner, "min"),
                    current(&min),
                    parse_number,
                ));
                meta = meta.or(field(
                    ui,
                    inspector,
                    (&owner, "max"),
                    current(&max),
                    parse_number,
                ));
            }
            if curved {
                ui.label("curve");
                let mut curve = match port.curve.as_deref() {
                    Some("exponential") => "exp",
                    Some("linear") => "lin",
                    _ => "",
                }
                .to_string();
                let was = curve.clone();
                egui::ComboBox::from_id_salt(("pipe curve", name))
                    .selected_text(&curve)
                    .show_ui(ui, |ui| {
                        for token in ["lin", "exp"] {
                            ui.selectable_value(&mut curve, token.to_string(), token);
                        }
                    });
                ui.end_row();
                if curve != was {
                    let curve = Meta {
                        curve: Some(curve),
                        ..Meta::default()
                    };
                    meta = meta.or(Some(Ok((curve, top.hash.clone()))));
                }
            }
            meta = meta.or(field(
                ui,
                inspector,
                (&owner, "unit"),
                current(&port.unit),
                parse_unit,
            ));
            if port.kind == "signal" {
                let channel = port.channel.map(|c| c.to_string()).unwrap_or_default();
                meta = meta.or(field(
                    ui,
                    inspector,
                    (&owner, "channel"),
                    current(&channel),
                    parse_channel,
                ));
            }
            meta
        })
        .inner;
    asks.extend(typed(meta, |meta| PipeEdit::InputMeta {
        name: name.to_string(),
        meta,
    }));
    ui.separator();
    if ui.button("Remove input").clicked() {
        let edit = PipeEdit::RemoveInput(name.to_string());
        asks.push(Ask::Edit(Edit::Pipe(edit), None));
    }
}

/// The interface output pipe `name` of `top`, selected on the canvas.
pub fn output(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    top: &InstrumentStructure,
    name: &str,
    problems: &[Problem<'_>],
    asks: &mut Vec<Ask>,
) {
    let Some(pipe) = top.outputs.iter().find(|p| p.name == name) else {
        ui.weak(format!("No interface output {name} in the document."));
        return;
    };
    let InterfaceOutput {
        from,
        channel,
        unit,
        min,
        max,
        ..
    } = pipe;
    ui.heading(name);
    ui.weak("Interface output");
    listed(ui, problems);
    ui.separator();
    let owner = format!("<output>{name}");
    let fed = wire_source(from);
    let meta = egui::Grid::new("output pipe")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            let mut meta = None;
            ui.label("fed from");
            let mut picked = fed.clone();
            egui::ComboBox::from_id_salt(("output feed", name))
                .selected_text(&picked)
                .show_ui(ui, |ui| {
                    for feed in feeds(top) {
                        ui.selectable_value(&mut picked, feed.clone(), feed);
                    }
                });
            ui.end_row();
            if picked != fed {
                let edit = PipeEdit::Feed {
                    name: name.to_string(),
                    from: picked,
                };
                asks.push(Ask::Edit(Edit::Pipe(edit), None));
            }
            let current = |text| Current {
                text,
                hash: &top.hash,
            };
            let (min, max) = (number_text(*min), number_text(*max));
            let unit = unit.clone().unwrap_or_default();
            let channel = channel.map(|c| c.to_string()).unwrap_or_default();
            meta = meta.or(field(
                ui,
                inspector,
                (&owner, "min"),
                current(&min),
                parse_number,
            ));
            meta = meta.or(field(
                ui,
                inspector,
                (&owner, "max"),
                current(&max),
                parse_number,
            ));
            meta = meta.or(field(
                ui,
                inspector,
                (&owner, "unit"),
                current(&unit),
                parse_unit,
            ));
            meta = meta.or(field(
                ui,
                inspector,
                (&owner, "channel"),
                current(&channel),
                parse_channel,
            ));
            meta
        })
        .inner;
    asks.extend(typed(meta, |meta| PipeEdit::OutputMeta {
        name: name.to_string(),
        meta,
    }));
    ui.separator();
    if ui.button("Remove output").clicked() {
        let edit = PipeEdit::RemoveOutput(name.to_string());
        asks.push(Ask::Edit(Edit::Pipe(edit), None));
    }
}

/// Metadata typed into a row, and the hash to write it against; or why what was typed is none.
type Typed = Result<(Meta, String), String>;

/// One metadata row: a label and a text box. Returns what was typed once a change is typed in and
/// the box let go of.
fn field(
    ui: &mut egui::Ui,
    inspector: &mut Inspector,
    (owner, name): (&str, &'static str),
    current: Current<'_>,
    parse: fn(&str, &str) -> Result<Meta, String>,
) -> Option<Typed> {
    let typed = text(ui, inspector, name, (owner, name), current, false);
    ui.end_row();
    let (typed, hash) = typed?;
    Some(parse(name, typed.trim()).map(|meta| (meta, hash)))
}

/// The ask a typed metadata row makes of the pipe that `edit` builds the change for.
fn typed(typed: Option<Typed>, edit: impl FnOnce(Meta) -> PipeEdit) -> Option<Ask> {
    Some(match typed? {
        Ok((meta, hash)) => Ask::Edit(Edit::Pipe(edit(meta)), Some(hash)),
        Err(why) => Ask::Refuse(why),
    })
}

fn number_text(value: Option<f64>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

fn cleared(name: &str) -> String {
    format!("A pipe's {name} can be changed, not cleared.")
}

fn parse_number(name: &str, text: &str) -> Result<Meta, String> {
    if text.is_empty() {
        return Err(cleared(name));
    }
    let number = (text.parse::<f64>().ok())
        .filter(|n| n.is_finite())
        .ok_or_else(|| format!("{name}: `{text}` is not a number"))?;
    Ok(match name {
        "min" => Meta {
            min: Some(number),
            ..Meta::default()
        },
        _ => Meta {
            max: Some(number),
            ..Meta::default()
        },
    })
}

fn parse_unit(name: &str, text: &str) -> Result<Meta, String> {
    if text.is_empty() {
        return Err(cleared(name));
    }
    Ok(Meta {
        unit: Some(text.to_string()),
        ..Meta::default()
    })
}

fn parse_channel(name: &str, text: &str) -> Result<Meta, String> {
    if text.is_empty() {
        return Err(cleared(name));
    }
    let channel =
        (text.parse::<usize>()).map_err(|_| format!("{name}: `{text}` is not a channel number"))?;
    Ok(Meta {
        channel: Some(channel),
        ..Meta::default()
    })
}

#[cfg(test)]
pub(super) mod tests {
    use serde_json::json;

    use super::*;

    pub fn structure(value: serde_json::Value) -> InstrumentStructure {
        serde_json::from_value(value).expect("a structure answer")
    }

    fn chain() -> InstrumentStructure {
        structure(json!({
            "instrument": "t", "hash": "h", "resources": [], "inputs": [],
            "report": { "ok": true, "errors": [], "warnings": [] },
            "nodes": [
                { "address": "/osc", "type_name": "oscillator",
                  "ports": { "inputs": [], "outputs": [ { "name": "audio", "kind": "signal" } ] } },
                { "address": "/env", "type_name": "envelope",
                  "ports": { "inputs": [], "outputs": [ { "name": "level", "kind": "value" } ] } },
                { "address": "/amp", "type_name": "mul_f32_signal",
                  "ports": { "inputs": [], "outputs": [ { "name": "out", "kind": "signal" } ] } }
            ],
            "outputs": [
                { "name": "out", "from": { "kind": "node_output", "node": "/amp", "port": "out" } }
            ]
        }))
    }

    #[test]
    fn a_new_output_is_fed_from_the_last_signal_unless_another_is_picked() {
        let top = chain();
        let mut adding = Adding::default();
        assert_eq!(
            adding.output(&top),
            Ok(PipeEdit::AddOutput {
                name: "out_2".into(),
                from: "/amp.out".into()
            })
        );
        adding.output_name = " main ".into();
        adding.output_from = Some("/osc.audio".into());
        assert_eq!(
            adding.output(&top),
            Ok(PipeEdit::AddOutput {
                name: "main".into(),
                from: "/osc.audio".into()
            })
        );
        assert_eq!(feeds(&top), ["/osc.audio", "/env.level", "/amp.out"]);
    }

    #[test]
    fn an_input_pipe_can_feed_an_output_but_is_never_its_default_feed() {
        let mut top = chain();
        top.inputs = serde_json::from_value(json!([
            { "port": { "name": "pitch", "kind": "value" } }
        ]))
        .unwrap();
        assert_eq!(feeds(&top)[0], "/pitch");
        assert_eq!(default_feed(&top).as_deref(), Some("/amp.out"));
        top.nodes.clear();
        assert_eq!(default_feed(&top), None);
    }

    #[test]
    fn an_enum_pipe_with_no_value_offers_its_variants_with_none_picked() {
        let port = serde_json::from_value(json!({
            "name": "shape", "kind": "enum", "variants": ["Sine", "Saw"]
        }))
        .unwrap();
        let field = pipe_field(&port);
        assert_eq!(field.target.address, "/shape");
        assert_eq!(
            field.control,
            super::super::model::Control::Choice {
                variants: vec!["Sine".into(), "Saw".into()],
                value: String::new()
            }
        );
    }

    #[test]
    fn an_output_needs_a_node_to_be_fed_from() {
        let empty = structure(json!({
            "instrument": "t", "hash": "h", "resources": [], "inputs": [], "outputs": [],
            "nodes": [], "report": { "ok": true, "errors": [], "warnings": [] }
        }));
        assert!(Adding::default().output(&empty).is_err());
    }

    #[test]
    fn a_new_input_needs_a_name_and_takes_a_number_or_a_symbol_as_its_value() {
        let mut adding = Adding::default();
        assert!(adding.input().is_err());
        adding.input_name = "/cutoff".into();
        assert_eq!(
            adding.input(),
            Ok(PipeEdit::AddInput {
                name: "cutoff".into(),
                type_name: "f32".into(),
                value: None
            })
        );
        adding.input_value = "440".into();
        assert!(matches!(
            adding.input(),
            Ok(PipeEdit::AddInput { value: Some(v), .. }) if v == json!(440.0)
        ));
        adding.input_type = "Waveform".into();
        adding.input_value = "Saw".into();
        assert!(matches!(
            adding.input(),
            Ok(PipeEdit::AddInput { value: Some(v), .. }) if v == json!("Saw")
        ));
    }

    #[test]
    fn metadata_is_set_one_field_at_a_time_and_never_cleared() {
        assert_eq!(
            parse_number("min", "20"),
            Ok(Meta {
                min: Some(20.0),
                ..Meta::default()
            })
        );
        assert_eq!(
            parse_number("max", "2e4"),
            Ok(Meta {
                max: Some(20000.0),
                ..Meta::default()
            })
        );
        assert!(parse_number("min", "").is_err());
        assert!(parse_number("min", "loud").is_err());
        assert!(parse_number("max", "inf").is_err());
        assert!(parse_unit("unit", "").is_err());
        assert!(parse_channel("channel", "-1").is_err());
        assert_eq!(
            parse_channel("channel", "1"),
            Ok(Meta {
                channel: Some(1),
                ..Meta::default()
            })
        );
    }
}
