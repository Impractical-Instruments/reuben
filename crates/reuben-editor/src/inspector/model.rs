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
    /// The node is a `subpatch`, whose ports are its child's interface pipes.
    pub pipe: bool,
}

impl Target {
    /// The live control message that sets this input to `value`, or `None` for a constant.
    ///
    /// An operator's input is addressed `/<node>/<input>`. A subpatch's port is the child's
    /// interface pipe, a node of its own at `/<node>/<pipe>` that takes control on its `in`.
    pub fn live(&self, value: f64) -> Option<ControlSendMessage> {
        let address = if self.pipe {
            format!("{}/{}/in", self.address, self.name)
        } else {
            format!("{}/{}", self.address, self.name)
        };
        (!self.constant).then(|| ControlSendMessage {
            address,
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

/// The widest linear range a slider spans. Across a slider about 180 points long this is some
/// 550 a point, already coarse; the registry's `±1000000`, its spelling of "no musical bound",
/// is twenty times past it.
const WIDEST_LINEAR: f64 = 1e5;

/// The most decades a logarithmic slider spans: 20 Hz to 20 kHz is three.
const MOST_DECADES: f64 = 6.0;

/// The fraction of its own size a dragged number moves per point: 2800 moves 14 a point, so a few
/// hundred hertz is a short drag either way.
const DRAG_FRACTION: f64 = 0.005;

/// How far a dragged number moves per point at least, so one at or near zero still moves.
const DRAG_FLOOR: f64 = 0.01;

/// How a number is set by pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Widget {
    /// A slider across `min..=max`.
    Slider {
        min: f64,
        max: f64,
        logarithmic: bool,
    },
    /// A value dragged by an amount that grows with its size, kept inside whichever bounds are
    /// declared when it is changed.
    Drag { min: Option<f64>, max: Option<f64> },
}

/// The widget for a number with the declared range `min..=max` and curve.
///
/// A range only gets a slider when a slider can sweep it usefully: both ends declared, and no
/// wider than [`WIDEST_LINEAR`] linearly or [`MOST_DECADES`] logarithmically. Anything else is
/// treated as unbounded.
pub fn widget(min: Option<f64>, max: Option<f64>, exponential: bool) -> Widget {
    let drag = Widget::Drag { min, max };
    let (Some(lo), Some(hi)) = (min, max) else {
        return drag;
    };
    if lo.partial_cmp(&hi) != Some(std::cmp::Ordering::Less) {
        return drag;
    }
    // A logarithmic slider cannot start at zero.
    let logarithmic = exponential && lo > 0.0;
    let fits = if logarithmic {
        (hi / lo).log10() <= MOST_DECADES
    } else {
        hi - lo <= WIDEST_LINEAR
    };
    if fits {
        Widget::Slider {
            min: lo,
            max: hi,
            logarithmic,
        }
    } else {
        drag
    }
}

/// How far a dragged number now at `value` moves per point of pointer travel.
pub fn drag_speed(value: f64, integer: bool) -> f64 {
    let floor = if integer { 0.1 } else { DRAG_FLOOR };
    (value.abs() * DRAG_FRACTION).max(floor)
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
        pipe: node.type_name == "subpatch",
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

/// The value row of the interface input pipe `pipe`: the pipe is a node at `/<name>` whose one
/// input `in` holds what the pipe gives when nothing feeds it.
///
/// A number pipe declaring no value is shown at zero, so that it can be given one.
pub fn pipe_field(pipe: &PortInfo) -> Field {
    let target = Target {
        address: format!("/{}", pipe.name),
        name: PIPE_INPUT.to_string(),
        constant: false,
        integer: pipe.kind == "int",
        pipe: false,
    };
    let control = match &pipe.default {
        Some(value) => literal_control(pipe, value),
        None if pipe.variants.is_empty() => match literal_control(pipe, &json!(0.0)) {
            number @ Control::Number { .. } => number,
            _ => Control::Shown {
                text: "nothing until it is fed".to_string(),
            },
        },
        // Nothing picked yet; picking writes the value as for a pipe that declares one.
        None => Control::Choice {
            variants: pipe.variants.clone(),
            value: String::new(),
        },
    };
    Field {
        target,
        kind: pipe.kind.clone(),
        control,
        set: pipe.default.is_some(),
    }
}

/// The input every interface input pipe takes its value on.
const PIPE_INPUT: &str = "in";

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
                pipe: false,
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
    /// Write this value to the document. `expect` is the content hash the value was set against:
    /// the document as it was when the number was taken up, or `None` for the one on screen now.
    Commit {
        target: Target,
        value: f64,
        expect: Option<String>,
    },
}

/// The number being held, while a slider or drag value is held down.
#[derive(Debug, Clone, PartialEq)]
struct Held {
    target: Target,
    value: f64,
    /// What the document held when the number was taken up, as this target holds it.
    from: f64,
    /// The content hash of the document when the number was taken up. The release is written
    /// against it, so a document changed under the hold is a conflict, not overwritten.
    hash: String,
}

impl Held {
    fn commit(self) -> Option<Step> {
        (self.value != self.from).then_some(Step::Commit {
            target: self.target,
            value: self.value,
            expect: Some(self.hash),
        })
    }
}

/// Drag, then commit: while a number is held its moves go to the engine live and the document is
/// not written; letting go writes the value it was let go at, once, unless that is the value the
/// document held when it was taken up.
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

    /// One frame of one number field: the document, whose content hash is `hash`, holds
    /// `document`; the field shows `value`; the pointer is or is not `holding` it; and it did or did
    /// not change this frame.
    pub fn number(
        &mut self,
        target: &Target,
        document: (f64, &str),
        value: f64,
        holding: bool,
        changed: bool,
    ) -> Vec<Step> {
        let (document, hash) = (target.quantize(document.0), document.1);
        let value = target.quantize(value);
        let mine = self.value(target).is_some();
        if holding {
            let mut steps = Vec::new();
            if !mine {
                // Another number still held is let go of before this one is taken up.
                steps.extend(self.release());
            }
            let held = self.held.get_or_insert_with(|| Held {
                target: target.clone(),
                value: document,
                from: document,
                hash: hash.to_string(),
            });
            let moved = changed && value != held.value;
            held.value = value;
            self.claimed = true;
            if moved && !target.constant {
                steps.push(Step::Live(target.clone(), value));
            }
            steps
        } else if mine {
            self.release().into_iter().collect()
        } else if changed && value != document {
            vec![Step::Commit {
                target: target.clone(),
                value,
                expect: None,
            }]
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

    /// Drop the held number without writing it: there is no document to write it to.
    pub fn cancel(&mut self) {
        self.held = None;
    }

    fn release(&mut self) -> Option<Step> {
        self.held.take().and_then(Held::commit)
    }
}

/// A text box being typed into.
#[derive(Debug, Clone, PartialEq)]
struct Draft {
    /// Whose text: a node's address, or empty for the instrument's own.
    owner: String,
    field: &'static str,
    text: String,
    /// What the document held for the field when the draft began.
    base: String,
    /// The content hash of the last document seen holding `base` for the field; the text is
    /// written against it.
    hash: String,
}

impl Draft {
    /// Take `hash` while the document still holds `base` for the field. A write elsewhere in the
    /// document, such as the box focus left writing the frame this one took it, is then no
    /// conflict; a change to this field still is.
    fn follow(&mut self, current: &str, hash: &str) {
        if current == self.base {
            self.hash = hash.to_string();
        }
    }
}

/// What one text box did this frame.
#[derive(Debug, Clone, PartialEq)]
pub struct Typing {
    /// What the box holds after this frame's keys.
    pub text: String,
    pub focused: bool,
    /// The box gave up focus this frame.
    pub lost: bool,
    /// Escape was pressed this frame.
    pub escaped: bool,
}

/// Type, then commit: what is typed into a text box is kept here, and written once, when the box
/// loses focus with something new in it. Escape, or the box no longer being drawn, drops it.
///
/// Each frame starts with [`begin`](Self::begin), reports every text box drawn through
/// [`frame`](Self::frame), and ends with [`end`](Self::end).
#[derive(Debug, Default)]
pub struct Drafts {
    /// Each with whether its box was drawn this frame. Focus moving from one box to another
    /// leaves two for a frame: the box taking focus, and the one losing it.
    drafts: Vec<(Draft, bool)>,
}

impl Drafts {
    pub fn begin(&mut self) {
        for (_, drawn) in &mut self.drafts {
            *drawn = false;
        }
    }

    fn find(&self, owner: &str, field: &str) -> Option<usize> {
        (self.drafts.iter()).position(|(d, _)| d.owner == owner && d.field == field)
    }

    /// What `owner`'s `field` box holds while it is being typed into.
    pub fn text(&self, owner: &str, field: &str) -> Option<&str> {
        self.find(owner, field)
            .map(|i| self.drafts[i].0.text.as_str())
    }

    /// One frame of `owner`'s `field` box, over a document whose content hash is `hash` and which
    /// holds `current` for it. Returns the text to write and the hash to write it against.
    pub fn frame(
        &mut self,
        owner: &str,
        field: &'static str,
        current: &str,
        hash: &str,
        typing: Typing,
    ) -> Option<(String, String)> {
        let found = self.find(owner, field);
        if typing.focused {
            match found {
                Some(i) => {
                    let draft = &mut self.drafts[i].0;
                    draft.follow(current, hash);
                    draft.text = typing.text;
                    self.drafts[i].1 = true;
                }
                None => self.drafts.push((
                    Draft {
                        owner: owner.to_string(),
                        field,
                        text: typing.text,
                        base: current.to_string(),
                        hash: hash.to_string(),
                    },
                    true,
                )),
            }
            return None;
        }
        let (mut draft, _) = self.drafts.remove(found?);
        draft.follow(current, hash);
        (typing.lost && !typing.escaped && typing.text != current)
            .then_some((typing.text, draft.hash))
    }

    /// End the frame, dropping every draft whose box was not drawn.
    pub fn end(&mut self) {
        self.drafts.retain(|(_, drawn)| *drawn);
    }

    pub fn cancel(&mut self) {
        self.drafts.clear();
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
            pipe: false,
        }
    }

    fn voices() -> Target {
        Target {
            address: "/filter".to_string(),
            name: "voices".to_string(),
            constant: true,
            integer: true,
            pipe: false,
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

    #[test]
    fn a_subpatch_port_goes_live_at_its_pipe() {
        let mut node = filter();
        node.type_name = "subpatch".to_string();
        let cutoff = fields(&node)
            .into_iter()
            .find(|f| f.target.name == "cutoff")
            .expect("a cutoff port")
            .target;
        assert!(cutoff.pipe);
        let live = cutoff.live(440.0).expect("a runtime input");
        assert_eq!(live.address, "/filter/cutoff/in");
    }

    fn live(value: f64) -> Step {
        Step::Live(cutoff(), value)
    }

    fn commit(target: Target, value: f64, expect: Option<&str>) -> Step {
        Step::Commit {
            target,
            value,
            expect: expect.map(str::to_string),
        }
    }

    /// What the document holds for every target in these tests, and its hash.
    const DOCUMENT: f64 = 300.0;
    const HASH: &str = "h0";

    /// One frame in which only `target`'s field is drawn, over a document holding `document`.
    fn frame_on(
        holds: &mut Holds,
        target: &Target,
        document: (f64, &str),
        value: f64,
        holding: bool,
        changed: bool,
    ) -> Vec<Step> {
        holds.begin();
        let mut steps = holds.number(target, document, value, holding, changed);
        steps.extend(holds.end());
        steps
    }

    fn frame(
        holds: &mut Holds,
        target: &Target,
        value: f64,
        holding: bool,
        changed: bool,
    ) -> Vec<Step> {
        frame_on(holds, target, (DOCUMENT, HASH), value, holding, changed)
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
        assert_eq!(release, [commit(target.clone(), 1600.0, Some(HASH))]);
        assert_eq!(holds.value(&target), None);
        assert_eq!(frame(&mut holds, &target, 1600.0, false, false), []);
    }

    #[test]
    fn a_release_is_written_against_the_document_as_it_was_when_taken_up() {
        let mut holds = Holds::default();
        let target = cutoff();
        frame(&mut holds, &target, 900.0, true, true);
        // Another writer changes the document under the hold, to the very value held.
        frame_on(&mut holds, &target, (900.0, "h1"), 900.0, true, false);
        assert_eq!(
            frame_on(&mut holds, &target, (900.0, "h1"), 900.0, false, false),
            [commit(target, 900.0, Some(HASH))]
        );
    }

    #[test]
    fn a_held_frame_that_does_not_move_sends_nothing() {
        let mut holds = Holds::default();
        let target = cutoff();
        frame(&mut holds, &target, 500.0, true, true);
        assert_eq!(frame(&mut holds, &target, 500.0, true, true), []);
    }

    #[test]
    fn a_click_that_does_not_move_writes_nothing_however_finely_the_document_is_spelled() {
        let mut holds = Holds::default();
        let target = cutoff();
        let fine = (0.1234567, HASH);
        assert_eq!(
            frame_on(&mut holds, &target, fine, 0.1234567, true, false),
            []
        );
        assert_eq!(
            frame_on(&mut holds, &target, fine, 0.1234567, false, false),
            []
        );
        assert_eq!(
            frame_on(&mut holds, &target, fine, 0.1234567, false, true),
            []
        );
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
            [commit(target, 5.0, Some(HASH))]
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
    fn a_change_made_without_holding_commits_at_once_against_the_document_on_screen() {
        let mut holds = Holds::default();
        let target = cutoff();
        assert_eq!(
            frame(&mut holds, &target, 512.0, false, true),
            [commit(target, 512.0, None)]
        );
    }

    #[test]
    fn a_held_number_whose_field_is_not_drawn_commits_at_its_last_value() {
        let mut holds = Holds::default();
        let target = cutoff();
        frame(&mut holds, &target, 700.0, true, true);
        holds.begin();
        assert_eq!(holds.end(), Some(commit(target, 700.0, Some(HASH))));
        holds.begin();
        assert_eq!(holds.end(), None);
    }

    #[test]
    fn a_cancelled_hold_is_never_written() {
        let mut holds = Holds::default();
        let target = cutoff();
        frame(&mut holds, &target, 700.0, true, true);
        holds.cancel();
        assert_eq!(holds.value(&target), None);
        holds.begin();
        assert_eq!(holds.end(), None);
        assert_eq!(frame(&mut holds, &target, DOCUMENT, false, false), []);
    }

    #[test]
    fn taking_up_another_number_lets_go_of_the_one_held() {
        let mut holds = Holds::default();
        frame(&mut holds, &cutoff(), 700.0, true, true);
        holds.begin();
        let steps = holds.number(&voices(), (DOCUMENT, HASH), 3.0, true, true);
        assert_eq!(steps, [commit(cutoff(), 700.0, Some(HASH))]);
        assert_eq!(holds.end(), None);
        assert_eq!(holds.value(&voices()), Some(3.0));
    }

    fn typing(text: &str, focused: bool, lost: bool, escaped: bool) -> Typing {
        Typing {
            text: text.to_string(),
            focused,
            lost,
            escaped,
        }
    }

    /// One frame in which only the `/filter` description box is drawn, over `hash`.
    fn typed(drafts: &mut Drafts, hash: &str, typing: Typing) -> Option<(String, String)> {
        drafts.begin();
        let edit = drafts.frame("/filter", "description", "old", hash, typing);
        drafts.end();
        edit
    }

    #[test]
    fn typed_text_is_written_once_when_the_box_loses_focus() {
        let mut drafts = Drafts::default();
        assert_eq!(
            typed(&mut drafts, "h0", typing("old", true, false, false)),
            None
        );
        assert_eq!(
            typed(&mut drafts, "h1", typing("new", true, false, false)),
            None
        );
        assert_eq!(drafts.text("/filter", "description"), Some("new"));
        assert_eq!(
            typed(&mut drafts, "h1", typing("new", false, true, false)),
            Some(("new".to_string(), "h1".to_string()))
        );
        assert_eq!(drafts.text("/filter", "description"), None);
        assert_eq!(
            typed(&mut drafts, "h1", typing("old", false, false, false)),
            None
        );
    }

    #[test]
    fn escape_drops_what_was_typed() {
        let mut drafts = Drafts::default();
        typed(&mut drafts, "h0", typing("new", true, false, false));
        assert_eq!(
            typed(&mut drafts, "h0", typing("new", false, true, true)),
            None
        );
        assert_eq!(drafts.text("/filter", "description"), None);
    }

    #[test]
    fn losing_focus_with_nothing_new_writes_nothing() {
        let mut drafts = Drafts::default();
        typed(&mut drafts, "h0", typing("old", true, false, false));
        assert_eq!(
            typed(&mut drafts, "h0", typing("old", false, true, false)),
            None
        );
    }

    #[test]
    fn a_draft_whose_box_is_not_drawn_is_dropped_and_never_written() {
        let mut drafts = Drafts::default();
        typed(&mut drafts, "h0", typing("new", true, false, false));
        // The node goes away under the box: a frame draws no box for it.
        drafts.begin();
        drafts.end();
        assert_eq!(drafts.text("/filter", "description"), None);
        // A box for the same address drawn later starts from the document, and writes nothing.
        assert_eq!(
            typed(&mut drafts, "h1", typing("old", false, true, false)),
            None
        );
    }

    #[test]
    fn focus_moving_to_another_box_writes_the_first_and_keeps_typing_in_the_second() {
        let mut drafts = Drafts::default();
        typed(&mut drafts, "h0", typing("new", true, false, false));
        drafts.begin();
        let name = drafts.frame("", "name", "inst", "h0", typing("inst", true, false, false));
        let description = drafts.frame(
            "/filter",
            "description",
            "old",
            "h0",
            typing("new", false, true, false),
        );
        drafts.end();
        assert_eq!(name, None);
        assert_eq!(description, Some(("new".to_string(), "h0".to_string())));
        assert_eq!(drafts.text("/filter", "description"), None);
        assert_eq!(drafts.text("", "name"), Some("inst"));
    }

    #[test]
    fn a_write_elsewhere_in_the_document_is_followed_and_one_to_the_field_is_not() {
        let frame = |drafts: &mut Drafts, current: &str, hash: &str, typing: Typing| {
            drafts.begin();
            let edit = drafts.frame("/filter", "description", current, hash, typing);
            drafts.end();
            edit
        };
        // Another box's write lands after focus arrives, and the first key comes the next frame.
        let mut drafts = Drafts::default();
        frame(&mut drafts, "old", "h0", typing("old", true, false, false));
        frame(&mut drafts, "old", "h1", typing("new", true, false, false));
        assert_eq!(
            frame(&mut drafts, "old", "h1", typing("new", false, true, false)),
            Some(("new".to_string(), "h1".to_string()))
        );
        // Something else writes this field while it is typed into: the write keeps the hash the
        // field was last seen unchanged at, so it is refused as a conflict.
        let mut drafts = Drafts::default();
        frame(&mut drafts, "old", "h0", typing("new", true, false, false));
        frame(
            &mut drafts,
            "theirs",
            "h1",
            typing("new", true, false, false),
        );
        assert_eq!(
            frame(
                &mut drafts,
                "theirs",
                "h1",
                typing("new", false, true, false)
            ),
            Some(("new".to_string(), "h0".to_string()))
        );
    }

    #[test]
    fn a_musical_range_is_a_slider_and_the_registrys_unbounded_range_is_dragged() {
        assert_eq!(
            widget(Some(20.0), Some(20000.0), true),
            Widget::Slider {
                min: 20.0,
                max: 20000.0,
                logarithmic: true
            }
        );
        assert_eq!(
            widget(Some(0.0), Some(1.0), false),
            Widget::Slider {
                min: 0.0,
                max: 1.0,
                logarithmic: false
            }
        );
        assert!(matches!(
            widget(Some(-1000.0), Some(20000.0), false),
            Widget::Slider { .. }
        ));
        assert_eq!(
            widget(Some(-1e6), Some(1e6), false),
            Widget::Drag {
                min: Some(-1e6),
                max: Some(1e6)
            }
        );
    }

    #[test]
    fn a_range_a_slider_cannot_sweep_is_dragged() {
        for (min, max, exponential) in [
            (Some(0.0), Some(1e6), true),
            (Some(1e-3), Some(1e6), true),
            (Some(0.0), None, false),
            (None, Some(1.0), false),
            (None, None, false),
            (Some(1.0), Some(1.0), false),
            (Some(2.0), Some(1.0), false),
        ] {
            assert_eq!(
                widget(min, max, exponential),
                Widget::Drag { min, max },
                "{min:?}..{max:?}"
            );
        }
    }

    #[test]
    fn a_drag_moves_in_proportion_to_the_value_and_never_stalls_at_zero() {
        let points_from_2800_to_2000 = 800.0 / drag_speed(2800.0, false);
        assert!(
            (20.0..=150.0).contains(&points_from_2800_to_2000),
            "{points_from_2800_to_2000}"
        );
        assert!(drag_speed(0.5, false) <= 0.01);
        assert_eq!(drag_speed(0.0, false), drag_speed(-0.0, false));
        assert!(drag_speed(0.0, false) > 0.0);
        assert_eq!(drag_speed(-2800.0, false), drag_speed(2800.0, false));
        assert!(drag_speed(0.0, true) >= 0.1, "a whole number still steps");
    }
}
