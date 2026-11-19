//! The inspector as data: which fields a node shows, and what holding, moving and letting go of a
//! number does. Nothing here draws or talks to the engine.

use reuben_api::authoring::{InputValue, PortInfo, StructureNode, WireSource};
use reuben_api::engine::ControlSendMessage;
use serde_json::{json, Value};

/// Significant figures a number is kept to on its way to the engine and the file. A slider's raw
/// position carries float noise below what anyone can hear or read; five figures resolve 20 Hz to
/// 20 kHz to a hundredth of a hertz at the bottom and 1 Hz at the top.
const FIGURES: i32 = 5;

/// One value a node holds that the inspector can set: a runtime input or a constant.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    pub address: String,
    pub name: String,
    /// A plan-time constant: it cannot be sent live, so it is only ever written.
    pub constant: bool,
    /// The port is an integer, so its value is written as one.
    pub integer: bool,
}

impl Target {
    /// The live control message that sets this input to `value`, or `None` for a constant.
    pub fn live(&self, value: f64) -> Option<ControlSendMessage> {
        (!self.constant).then(|| ControlSendMessage {
            address: format!("{}/{}", self.address, self.name),
            args: vec![json!(value)],
        })
    }

    /// `value` as the document spells it.
    pub fn literal(&self, value: f64) -> Value {
        if self.integer {
            json!(value as i64)
        } else {
            json!(value)
        }
    }

    /// `raw` as this target can hold it: whole for an integer, [`FIGURES`] figures otherwise.
    pub fn quantize(&self, raw: f64) -> f64 {
        if self.integer {
            raw.round()
        } else {
            significant(raw, FIGURES)
        }
    }
}

fn significant(value: f64, figures: i32) -> f64 {
    if value == 0.0 || !value.is_finite() {
        return value;
    }
    let scale = 10f64.powi(figures - 1 - value.abs().log10().floor() as i32);
    (value * scale).round() / scale
}

/// How a field is shown and set.
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    Number {
        value: f64,
        /// `None` when the port declares no bound on that side.
        min: Option<f64>,
        max: Option<f64>,
        exponential: bool,
        unit: String,
    },
    Choice {
        variants: Vec<String>,
        value: String,
    },
    /// Fed by a wire, so there is no literal to set here.
    Wired { from: String },
    /// Shown, not set: a value of a kind the inspector has no control for, or nothing at all.
    Shown { text: String },
}

/// One row of the inspector for a node.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub target: Target,
    pub kind: String,
    pub control: Control,
    /// The node sets this value; otherwise it takes the port's default.
    pub set: bool,
}

/// Every input and constant of `node`, in its ports' order: runtime inputs, then constants.
///
/// A node whose type is unknown has no ports, so what it sets is shown and nothing can be set.
pub fn fields(node: &StructureNode) -> Vec<Field> {
    if node.ports.inputs.is_empty() {
        return unported(node);
    }
    (node.ports.inputs.iter())
        .map(|port| field(node, port))
        .collect()
}

fn field(node: &StructureNode, port: &PortInfo) -> Field {
    let target = Target {
        address: node.address.clone(),
        name: port.name.clone(),
        constant: port.constant,
        integer: port.kind == "int",
    };
    let held = if port.constant {
        (node.constants.iter())
            .find(|c| c.name == port.name)
            .map(|c| InputValue::Literal {
                value: c.value.clone(),
            })
    } else {
        (node.inputs.iter())
            .find(|i| i.name == port.name)
            .map(|i| i.value.clone())
    };
    let set = held.is_some();
    let control = match held {
        Some(InputValue::Wire { from }) => Control::Wired {
            from: wire_source(&from),
        },
        Some(InputValue::Literal { value }) => literal_control(port, &value),
        None => match &port.default {
            Some(value) => literal_control(port, value),
            None => Control::Shown {
                text: "unwired".to_string(),
            },
        },
    };
    Field {
        target,
        kind: port.kind.clone(),
        control,
        set,
    }
}

fn literal_control(port: &PortInfo, value: &Value) -> Control {
    if !port.variants.is_empty() {
        let value = match value {
            Value::String(symbol) => symbol.clone(),
            // An in-range index is the other spelling of a variant.
            other => (other.as_u64())
                .and_then(|i| port.variants.get(i as usize).cloned())
                .unwrap_or_else(|| other.to_string()),
        };
        return Control::Choice {
            variants: port.variants.clone(),
            value,
        };
    }
    match (port.kind.as_str(), value.as_f64()) {
        ("value" | "signal" | "int", Some(number)) => Control::Number {
            value: number,
            min: port.min,
            max: port.max,
            exponential: port.curve.as_deref() == Some("exponential"),
            unit: port.unit.clone(),
        },
        _ => Control::Shown { text: shown(value) },
    }
}

fn unported(node: &StructureNode) -> Vec<Field> {
    let constants = node
        .constants
        .iter()
        .map(|c| (c.name.as_str(), true, c.value.clone()));
    let inputs = node.inputs.iter().map(|i| {
        let text = match &i.value {
            InputValue::Literal { value } => value.clone(),
            InputValue::Wire { from } => Value::String(format!("← {}", wire_source(from))),
        };
        (i.name.as_str(), false, text)
    });
    inputs
        .chain(constants)
        .map(|(name, constant, value)| Field {
            target: Target {
                address: node.address.clone(),
                name: name.to_string(),
                constant,
                integer: false,
            },
            kind: String::new(),
            control: Control::Shown {
                text: shown(&value),
            },
            set: true,
        })
        .collect()
}

fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The output a wire comes from, as an address names it.
pub fn wire_source(from: &WireSource) -> String {
    match from {
        WireSource::NodeOutput { node, port } => format!("{node}.{port}"),
        WireSource::InterfaceInput { name } => format!("/{name}"),
    }
}

/// What a number field asks for on a frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    /// Send this value of this input to the engine, and write nothing.
    Live(Target, f64),
    /// Write this value to the document.
    Commit(Target, f64),
}

/// The number being held, while a slider or drag value is held down.
#[derive(Debug, Clone, PartialEq)]
struct Held {
    target: Target,
    value: f64,
    /// What the document held when it was taken up.
    from: f64,
}

impl Held {
    fn commit(self) -> Option<Step> {
        (self.value != self.from).then_some(Step::Commit(self.target, self.value))
    }
}

/// Drag, then commit: while a number is held its moves go to the engine live and the document is
/// not written; letting go writes the value it was let go at, once, unless that is the value the
/// document already holds.
///
/// Each frame starts with [`begin`](Self::begin), reports every number field drawn through
/// [`number`](Self::number), and ends with [`end`](Self::end).
#[derive(Debug, Default)]
pub struct Holds {
    held: Option<Held>,
    /// A field reported the held number as still held this frame.
    claimed: bool,
}

impl Holds {
    pub fn begin(&mut self) {
        self.claimed = false;
    }

    /// The value `target` is held at, to draw in place of the document's.
    pub fn value(&self, target: &Target) -> Option<f64> {
        (self.held.as_ref())
            .filter(|held| &held.target == target)
            .map(|held| held.value)
    }

    /// One frame of one number field: the document holds `document`, the field shows `value`,
    /// the pointer is or is not `holding` it, and it did or did not change this frame.
    pub fn number(
        &mut self,
        target: &Target,
        document: f64,
        value: f64,
        holding: bool,
        changed: bool,
    ) -> Vec<Step> {
        let value = target.quantize(value);
        let mine = self.value(target).is_some();
        if holding {
            let mut steps = Vec::new();
            if !mine {
                // Another number still held is let go of before this one is taken up.
                steps.extend(self.release());
            }
            let before = self.value(target).unwrap_or(document);
            self.held = Some(Held {
                target: target.clone(),
                value,
                from: document,
            });
            self.claimed = true;
            if changed && value != before && !target.constant {
                steps.push(Step::Live(target.clone(), value));
            }
            steps
        } else if mine {
            self.release().into_iter().collect()
        } else if changed && value != document {
            vec![Step::Commit(target.clone(), value)]
        } else {
            Vec::new()
        }
    }

    /// End the frame. A number held last frame that no field reported held this frame — its
    /// field was not drawn — is let go of at the value it was last held at.
    pub fn end(&mut self) -> Option<Step> {
        if self.claimed {
            None
        } else {
            self.release()
        }
    }

    fn release(&mut self) -> Option<Step> {
        self.held.take().and_then(Held::commit)
    }
}

#[cfg(test)]
mod tests {
    use reuben_api::authoring::{NodeConstant, NodeInput, NodePorts};

    use super::*;

    fn port(name: &str, kind: &str) -> PortInfo {
        PortInfo {
            name: name.to_string(),
            kind: kind.to_string(),
            constant: false,
            default: None,
            min: None,
            max: None,
            unit: String::new(),
            curve: None,
            variants: Vec::new(),
            channel: None,
        }
    }

    fn cutoff_port() -> PortInfo {
        PortInfo {
            default: Some(json!(1000.0)),
            min: Some(20.0),
            max: Some(20000.0),
            unit: "Hz".to_string(),
            curve: Some("exponential".to_string()),
            ..port("cutoff", "signal")
        }
    }

    fn filter() -> StructureNode {
        StructureNode {
            address: "/filter".to_string(),
            type_name: "filter".to_string(),
            description: None,
            layout: None,
            constants: vec![NodeConstant {
                name: "voices".to_string(),
                value: json!(4),
            }],
            inputs: vec![
                NodeInput {
                    name: "audio".to_string(),
                    value: InputValue::Wire {
                        from: WireSource::NodeOutput {
                            node: "/osc".to_string(),
                            port: "audio".to_string(),
                        },
                    },
                },
                NodeInput {
                    name: "cutoff".to_string(),
                    value: InputValue::Literal {
                        value: json!(300.0),
                    },
                },
                NodeInput {
                    name: "mode".to_string(),
                    value: InputValue::Literal { value: json!(1) },
                },
            ],
            resources: Vec::new(),
            ports: NodePorts {
                inputs: vec![
                    port("audio", "signal"),
                    cutoff_port(),
                    PortInfo {
                        default: Some(json!(0.2)),
                        min: Some(0.0),
                        max: Some(1.0),
                        curve: Some("linear".to_string()),
                        ..port("resonance", "signal")
                    },
                    PortInfo {
                        default: Some(json!("Lp")),
                        variants: vec!["Lp".into(), "Hp".into(), "Bp".into()],
                        ..port("mode", "enum")
                    },
                    PortInfo {
                        constant: true,
                        default: Some(json!(1)),
                        min: Some(1.0),
                        max: Some(16.0),
                        ..port("voices", "int")
                    },
                ],
                outputs: vec![port("audio", "signal")],
            },
            child: None,
        }
    }

    fn cutoff() -> Target {
        Target {
            address: "/filter".to_string(),
            name: "cutoff".to_string(),
            constant: false,
            integer: false,
        }
    }

    fn voices() -> Target {
        Target {
            address: "/filter".to_string(),
            name: "voices".to_string(),
            constant: true,
            integer: true,
        }
    }

    #[test]
    fn a_node_shows_every_port_in_order_with_what_it_sets_or_the_default() {
        let fields = fields(&filter());
        let names: Vec<&str> = fields.iter().map(|f| f.target.name.as_str()).collect();
        assert_eq!(names, ["audio", "cutoff", "resonance", "mode", "voices"]);

        assert_eq!(
            fields[0].control,
            Control::Wired {
                from: "/osc.audio".to_string()
            }
        );
        assert_eq!(
            fields[1].control,
            Control::Number {
                value: 300.0,
                min: Some(20.0),
                max: Some(20000.0),
                exponential: true,
                unit: "Hz".to_string(),
            }
        );
        assert!(fields[1].set);
        assert!(
            matches!(fields[2].control, Control::Number { value, .. } if value == 0.2),
            "an unset input shows its default"
        );
        assert!(!fields[2].set);
        assert_eq!(
            fields[3].control,
            Control::Choice {
                variants: vec!["Lp".into(), "Hp".into(), "Bp".into()],
                value: "Hp".to_string(),
            },
            "an index is read as the variant it names"
        );
        assert_eq!(fields[4].target, voices());
        assert!(matches!(fields[4].control, Control::Number { value, .. } if value == 4.0));
    }

    #[test]
    fn a_port_with_no_literal_and_no_default_is_shown_not_set() {
        let mut node = filter();
        node.inputs.clear();
        let fields = fields(&node);
        assert!(matches!(&fields[0].control, Control::Shown { .. }));
        assert!(!fields[0].set);
    }

    #[test]
    fn a_node_of_unknown_type_shows_what_it_sets() {
        let mut node = filter();
        node.ports = NodePorts {
            inputs: Vec::new(),
            outputs: Vec::new(),
        };
        let fields = fields(&node);
        let shown: Vec<(&str, &Control)> = fields
            .iter()
            .map(|f| (f.target.name.as_str(), &f.control))
            .collect();
        assert_eq!(shown.len(), 4);
        assert_eq!(
            shown[0],
            (
                "audio",
                &Control::Shown {
                    text: "← /osc.audio".to_string()
                }
            )
        );
        assert_eq!(
            shown[1],
            (
                "cutoff",
                &Control::Shown {
                    text: "300.0".to_string()
                }
            )
        );
    }

    #[test]
    fn an_input_goes_live_at_its_node_and_port_and_a_constant_never_does() {
        let live = cutoff().live(440.0).expect("a runtime input");
        assert_eq!(live.address, "/filter/cutoff");
        assert_eq!(live.args, [json!(440.0)]);
        assert!(voices().live(4.0).is_none());
    }

    #[test]
    fn a_value_is_kept_to_five_figures_and_an_integer_to_a_whole_number() {
        assert_eq!(cutoff().quantize(1234.5678), 1234.6);
        assert_eq!(cutoff().quantize(0.123456789), 0.12346);
        assert_eq!(cutoff().quantize(0.0), 0.0);
        assert_eq!(voices().quantize(3.6), 4.0);
        assert_eq!(voices().literal(4.0), json!(4));
        assert_eq!(cutoff().literal(440.5), json!(440.5));
    }

    fn live(value: f64) -> Step {
        Step::Live(cutoff(), value)
    }

    /// What the document holds for every target in these tests.
    const DOCUMENT: f64 = 300.0;

    /// One frame in which only `target`'s field is drawn.
    fn frame(
        holds: &mut Holds,
        target: &Target,
        value: f64,
        holding: bool,
        changed: bool,
    ) -> Vec<Step> {
        holds.begin();
        let mut steps = holds.number(target, DOCUMENT, value, holding, changed);
        steps.extend(holds.end());
        steps
    }

    #[test]
    fn a_drag_sends_every_move_live_and_letting_go_commits_once_at_the_last_value() {
        let mut holds = Holds::default();
        let target = cutoff();
        let mut steps = Vec::new();
        steps.extend(frame(&mut holds, &target, 300.0, true, false));
        for value in [400.0, 800.0, 1600.0] {
            steps.extend(frame(&mut holds, &target, value, true, true));
            assert_eq!(holds.value(&target), Some(value));
        }
        assert_eq!(steps, [live(400.0), live(800.0), live(1600.0)]);

        let release = frame(&mut holds, &target, 1600.0, false, false);
        assert_eq!(release, [Step::Commit(target.clone(), 1600.0)]);
        assert_eq!(holds.value(&target), None);
        assert_eq!(frame(&mut holds, &target, 1600.0, false, false), []);
    }

    #[test]
    fn a_held_frame_that_does_not_move_sends_nothing() {
        let mut holds = Holds::default();
        let target = cutoff();
        frame(&mut holds, &target, 300.0, true, true);
        assert_eq!(frame(&mut holds, &target, 300.0, true, true), []);
    }

    #[test]
    fn a_held_constant_sends_nothing_and_commits_on_release() {
        let mut holds = Holds::default();
        let target = voices();
        assert_eq!(frame(&mut holds, &target, 2.0, true, true), []);
        assert_eq!(frame(&mut holds, &target, 5.4, true, true), []);
        assert_eq!(holds.value(&target), Some(5.0));
        assert_eq!(
            frame(&mut holds, &target, 5.0, false, false),
            [Step::Commit(target, 5.0)]
        );
    }

    #[test]
    fn letting_go_where_the_document_already_is_writes_nothing() {
        let mut holds = Holds::default();
        let target = cutoff();
        frame(&mut holds, &target, 900.0, true, true);
        frame(&mut holds, &target, DOCUMENT, true, true);
        assert_eq!(frame(&mut holds, &target, DOCUMENT, false, false), []);
        assert_eq!(frame(&mut holds, &target, DOCUMENT, false, true), []);
    }

    #[test]
    fn a_change_made_without_holding_commits_at_once() {
        let mut holds = Holds::default();
        let target = cutoff();
        assert_eq!(
            frame(&mut holds, &target, 512.0, false, true),
            [Step::Commit(target, 512.0)]
        );
    }

    #[test]
    fn a_held_number_whose_field_is_not_drawn_commits_at_its_last_value() {
        let mut holds = Holds::default();
        let target = cutoff();
        frame(&mut holds, &target, 700.0, true, true);
        holds.begin();
        assert_eq!(holds.end(), Some(Step::Commit(target, 700.0)));
        holds.begin();
        assert_eq!(holds.end(), None);
    }

    #[test]
    fn taking_up_another_number_lets_go_of_the_one_held() {
        let mut holds = Holds::default();
        frame(&mut holds, &cutoff(), 700.0, true, true);
        holds.begin();
        let steps = holds.number(&voices(), DOCUMENT, 3.0, true, true);
        assert_eq!(steps, [Step::Commit(cutoff(), 700.0)]);
        assert_eq!(holds.end(), None);
        assert_eq!(holds.value(&voices()), Some(3.0));
    }
}
