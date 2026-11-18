//! The panel as data: which controls it has, how they are laid out, and what each one sends.
//!
//! A panel is resolved from the instrument's boundary — the interface inputs, as
//! [`describe_boundary`](reuben_api::authoring::describe_boundary) reports them — either through a
//! [`SurfaceDoc`] that curates them or straight from the inputs themselves. Nothing here draws.

use reuben_api::authoring::{Boundary, PortInfo};
use reuben_api::engine::ControlSendMessage;
use serde_json::json;

use super::surface::{SurfaceControl, SurfaceDoc};

/// Widgets per row for controls with no `group`, when the surface doc gives no `cols`.
pub const DEFAULT_COLS: usize = 4;

/// Where a run of controls sharing one `group` wraps: a sequencer lane is 16 steps, and a lane
/// reads as a lane only on one row.
pub const GROUP_COLS: usize = 16;

/// The widget kinds this panel draws. A surface doc may name others (`xy-pad`, `grid`,
/// `visualizer`); those are skipped with a warning.
const FADER: &str = "fader";
const RADIAL: &str = "radial";
const PARAM_TOGGLE: &str = "param-toggle";
const NOTE_TOGGLE: &str = "note-toggle";
const CHORD_BUTTON: &str = "chord-button";
const KEYBOARD: &str = "keyboard";

/// The resolved panel for one instrument.
#[derive(Debug, Clone, PartialEq)]
pub struct Panel {
    pub controls: Vec<Control>,
    pub cols: usize,
    /// Everything resolving the panel skipped or adjusted, in the order it happened.
    pub warnings: Vec<String>,
}

/// One control: the interface input it drives and how it is presented.
#[derive(Debug, Clone, PartialEq)]
pub struct Control {
    /// The interface input's name. Several controls may share one.
    pub bind: String,
    pub label: String,
    pub group: Option<String>,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    Fader,
    Radial,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// A continuous or whole-numbered value.
    Value {
        shape: Shape,
        /// `None` when the input declares no bound on that side.
        min: Option<f64>,
        max: Option<f64>,
        rest: f64,
        /// The input is an integer control: the widget steps on whole values.
        integer: bool,
        exponential: bool,
        unit: String,
    },
    /// A latching on/off switch that sets the input to 1 or 0.
    Toggle { rest: bool },
    /// A latching button that holds one absolute note on a note input.
    NoteToggle { note: f64, velocity: f64 },
    /// A momentary button that plays one scale degree on a note input.
    ChordButton { degree: i32 },
    /// One of an enum input's variants.
    Choice { variants: Vec<String>, rest: usize },
    /// An on-screen keyboard playing absolute notes on a note input.
    Keyboard,
}

/// What the player did to a control.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    /// Moved a [`Kind::Value`] to this value.
    Set(f64),
    /// Switched a [`Kind::Toggle`] or [`Kind::NoteToggle`] on or off, or pressed or released a
    /// [`Kind::ChordButton`].
    Switch(bool),
    /// Picked this variant of a [`Kind::Choice`].
    Pick(usize),
    /// Pressed or released this MIDI note on a [`Kind::Keyboard`].
    Key { midi: u8, down: bool },
}

impl Control {
    /// Where every message for this control goes: the interface input is a node at `/<name>`, and
    /// it takes external control on its one port, `in`.
    pub fn address(&self) -> String {
        format!("/{}/in", self.bind)
    }

    /// The live control message `gesture` sends, or `None` when the gesture is not one this kind
    /// of control makes.
    ///
    /// A note is `[pitch, velocity]` and velocity 0 is the note-off. The pitch atom's type picks
    /// its meaning at the engine: a float is an absolute MIDI note, an integer a scale degree.
    pub fn message(&self, gesture: Gesture) -> Option<ControlSendMessage> {
        let args = match (&self.kind, gesture) {
            (
                Kind::Value {
                    min, max, integer, ..
                },
                Gesture::Set(value),
            ) => {
                let value = clamp(value, *min, *max);
                vec![json!(if *integer { value.round() } else { value })]
            }
            (Kind::Toggle { .. }, Gesture::Switch(on)) => vec![json!(gate(on))],
            (Kind::NoteToggle { note, velocity }, Gesture::Switch(on)) => {
                vec![json!(note), json!(if on { *velocity } else { 0.0 })]
            }
            (Kind::ChordButton { degree }, Gesture::Switch(on)) => {
                vec![json!(degree), json!(gate(on))]
            }
            (Kind::Choice { variants, .. }, Gesture::Pick(index)) => {
                vec![json!(variants.get(index)?)]
            }
            (Kind::Keyboard, Gesture::Key { midi, down }) => {
                vec![json!(f64::from(midi)), json!(gate(down))]
            }
            _ => return None,
        };
        Some(ControlSendMessage {
            address: self.address(),
            args,
        })
    }
}

fn gate(on: bool) -> f64 {
    if on {
        1.0
    } else {
        0.0
    }
}

fn clamp(value: f64, min: Option<f64>, max: Option<f64>) -> f64 {
    let value = min.map_or(value, |min| value.max(min));
    max.map_or(value, |max| value.min(max))
}

/// Where `value` sits in `min..=max` as a fraction, along the input's curve.
pub fn to_unit(value: f64, min: f64, max: f64, exponential: bool) -> f64 {
    if max <= min {
        return 0.0;
    }
    let value = value.clamp(min, max);
    if exponential && min > 0.0 {
        (value / min).ln() / (max / min).ln()
    } else {
        (value - min) / (max - min)
    }
}

/// The inverse of [`to_unit`].
pub fn from_unit(unit: f64, min: f64, max: f64, exponential: bool) -> f64 {
    let unit = unit.clamp(0.0, 1.0);
    if exponential && min > 0.0 && max > min {
        min * (max / min).powf(unit)
    } else {
        min + (max - min) * unit
    }
}

/// The default label for an input: underscores become spaces and each word's first letter is
/// uppercased (`kick_step1` → `Kick Step1`). Every surface-doc reader derives it the same way, so
/// the editor's labels match the other surfaces'.
pub fn default_label(name: &str) -> String {
    name.replace('_', " ")
        .split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

fn number(port: &PortInfo) -> Option<f64> {
    port.default.as_ref().and_then(serde_json::Value::as_f64)
}

/// Whether a value control can drive this input: a held scalar, an integer, or a signal with a
/// declared range. A signal with none is audio.
fn takes_a_value(port: &PortInfo) -> bool {
    match port.kind.as_str() {
        "value" | "int" => true,
        "signal" => port.min.is_some() && port.max.is_some(),
        _ => false,
    }
}

fn value_kind(port: &PortInfo, shape: Shape, min: Option<f64>, max: Option<f64>) -> Kind {
    Kind::Value {
        shape,
        min,
        max,
        rest: clamp(number(port).or(min).unwrap_or(0.0), min, max),
        integer: port.kind == "int",
        exponential: port.curve.as_deref() == Some("exponential"),
        unit: port.unit.clone(),
    }
}

impl Panel {
    /// The panel an instrument gets with no surface doc: one control per interface input, in the
    /// order the boundary lists them — a fader for a value, a selector for an enum, a keyboard for
    /// a note input.
    pub fn generated(boundary: &Boundary) -> Self {
        let mut controls = Vec::new();
        let mut warnings = Vec::new();
        for port in &boundary.inputs {
            let name = &port.name;
            let kind = if port.channel.is_some() {
                warnings.push(format!(
                    "input `{name}` reads an audio input channel and has no control"
                ));
                continue;
            } else if takes_a_value(port) {
                value_kind(port, Shape::Fader, port.min, port.max)
            } else if port.kind == "enum" && !port.variants.is_empty() {
                let rest = port
                    .default
                    .as_ref()
                    .and_then(serde_json::Value::as_str)
                    .and_then(|symbol| port.variants.iter().position(|v| v == symbol))
                    .unwrap_or(0);
                Kind::Choice {
                    variants: port.variants.clone(),
                    rest,
                }
            } else if port.kind == "message" {
                Kind::Keyboard
            } else {
                warnings.push(format!(
                    "input `{name}` ({}) has no control in a generated panel",
                    port.kind
                ));
                continue;
            };
            controls.push(Control {
                bind: name.clone(),
                label: default_label(name),
                group: None,
                kind,
            });
        }
        Self {
            controls,
            cols: DEFAULT_COLS,
            warnings,
        }
    }

    /// The panel a surface doc describes, resolved against the instrument's boundary.
    ///
    /// The doc carries presentation only; every range, default, unit and curve comes from the
    /// bound input. A control the panel cannot resolve is skipped with a warning and the rest of
    /// the doc still stands.
    pub fn from_surface(doc: &SurfaceDoc, boundary: &Boundary) -> Self {
        let mut warnings = Vec::new();
        if doc.instrument != boundary.instrument {
            warnings.push(format!(
                "the surface doc is for instrument `{}`, not `{}`",
                doc.instrument, boundary.instrument
            ));
        }
        let controls = doc
            .controls
            .iter()
            .filter_map(|control| match resolve(control, boundary) {
                Ok((resolved, adjusted)) => {
                    warnings.extend(adjusted);
                    Some(resolved)
                }
                Err(skipped) => {
                    warnings.push(skipped);
                    None
                }
            })
            .collect();
        Self {
            controls,
            cols: doc.cols.unwrap_or(DEFAULT_COLS),
            warnings,
        }
    }

    /// The controls as rows of indices into [`controls`](Self::controls), in order. A run of
    /// consecutive controls sharing a `group` gets rows of its own, wrapping at [`GROUP_COLS`];
    /// controls with no group flow [`cols`](Self::cols) to a row between them.
    pub fn rows(&self) -> Vec<Vec<usize>> {
        let mut rows = Vec::new();
        let mut start = 0;
        while start < self.controls.len() {
            let group = &self.controls[start].group;
            let len = self.controls[start..]
                .iter()
                .take_while(|c| &c.group == group)
                .count();
            let width = match group {
                Some(_) => GROUP_COLS,
                None => self.cols.max(1),
            };
            let run: Vec<usize> = (start..start + len).collect();
            rows.extend(run.chunks(width).map(<[usize]>::to_vec));
            start += len;
        }
        rows
    }
}

/// One surface-doc control resolved against the boundary, with any adjustment made to it — or why
/// it was skipped.
fn resolve(
    control: &SurfaceControl,
    boundary: &Boundary,
) -> Result<(Control, Vec<String>), String> {
    let bind = &control.bind;
    let Some(port) = boundary.inputs.iter().find(|p| &p.name == bind) else {
        return Err(format!(
            "surface control binds `{bind}`, which is not an interface input — skipped"
        ));
    };
    let label = control.label.clone().unwrap_or_else(|| default_label(bind));
    let widget = match control.widget.as_deref() {
        Some(widget) => widget,
        None if port.channel.is_none() && takes_a_value(port) => FADER,
        None => {
            return Err(format!(
                "surface control `{label}` binds `{bind}` ({}) with no `widget`, and that input \
                 has no default widget — skipped",
                port.kind
            ))
        }
    };
    let mut adjusted = Vec::new();
    let kind = match widget {
        FADER | RADIAL => {
            let mut narrow = |name: &str, given: Option<f64>, declared: Option<f64>| {
                let Some(given) = given else { return declared };
                let inside = clamp(given, port.min, port.max);
                if inside != given {
                    adjusted.push(format!(
                        "surface control `{label}`: {name} {given} is outside the input's range \
                         — using {inside}"
                    ));
                }
                Some(inside)
            };
            let min = narrow("min", control.min, port.min);
            let max = narrow("max", control.max, port.max);
            // A range with nothing in it clamps every send to one value while the widget still
            // shows travel; the input's own range is the honest fallback.
            let (min, max) = match (min, max) {
                (Some(low), Some(high)) if low > high => {
                    adjusted.push(format!(
                        "surface control `{label}`: min {low} is above max {high} — using the \
                         input's own range"
                    ));
                    (port.min, port.max)
                }
                range => range,
            };
            let shape = if widget == RADIAL {
                Shape::Radial
            } else {
                Shape::Fader
            };
            value_kind(port, shape, min, max)
        }
        PARAM_TOGGLE => Kind::Toggle {
            rest: number(port).is_some_and(|v| v >= 0.5),
        },
        NOTE_TOGGLE => Kind::NoteToggle {
            note: control.note.ok_or_else(|| {
                format!("note-toggle `{label}` (input `{bind}`) has no `note` — skipped")
            })? as f64,
            velocity: control.velocity.unwrap_or(1.0),
        },
        CHORD_BUTTON => Kind::ChordButton {
            degree: control
                .degree
                .and_then(|degree| i32::try_from(degree).ok())
                .ok_or_else(|| {
                    format!("chord-button `{label}` (input `{bind}`) has no `degree` — skipped")
                })?,
        },
        KEYBOARD => Kind::Keyboard,
        other => {
            return Err(format!(
                "surface control `{label}` asks for a `{other}` widget, which the editor does \
                 not draw — skipped"
            ))
        }
    };
    Ok((
        Control {
            bind: bind.clone(),
            label,
            group: control.group.clone(),
            kind,
        },
        adjusted,
    ))
}

/// One key of the on-screen keyboard, in units of one white key's width from the left edge.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    pub midi: u8,
    pub black: bool,
    pub left: f32,
    pub right: f32,
}

/// A black key's width, in white-key widths.
const BLACK_WIDTH: f32 = 0.6;

/// How far down the keyboard a black key reaches, as a fraction of its height.
pub const BLACK_DEPTH: f32 = 0.6;

fn is_black(midi: u8) -> bool {
    matches!(midi % 12, 1 | 3 | 6 | 8 | 10)
}

/// The keys from `low` up through `white_keys` white keys, each black key straddling the boundary
/// between its neighbours. `low` is taken as a white key.
pub fn keys(low: u8, white_keys: usize) -> Vec<Key> {
    let mut keys = Vec::new();
    let mut whites = 0;
    for midi in low..=127 {
        if is_black(midi) {
            let centre = whites as f32;
            keys.push(Key {
                midi,
                black: true,
                left: centre - BLACK_WIDTH / 2.0,
                right: centre + BLACK_WIDTH / 2.0,
            });
        } else {
            if whites == white_keys {
                break;
            }
            keys.push(Key {
                midi,
                black: false,
                left: whites as f32,
                right: whites as f32 + 1.0,
            });
            whites += 1;
        }
    }
    // A black key past the last white one would hang off the right edge.
    if keys.last().is_some_and(|k| k.black) {
        keys.pop();
    }
    keys
}

/// The key under a point: `x` in white-key widths, `y` as a fraction of the keyboard's height. A
/// black key wins where it overlaps the white keys beneath it.
pub fn key_at(keys: &[Key], x: f32, y: f32) -> Option<u8> {
    let under = |k: &&Key| k.left <= x && x < k.right;
    keys.iter()
        .filter(|k| k.black && y < BLACK_DEPTH)
        .find(under)
        .or_else(|| keys.iter().filter(|k| !k.black).find(under))
        .map(|k| k.midi)
}

#[cfg(test)]
mod tests {
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

    fn ranged(name: &str, kind: &str, default: f64, min: f64, max: f64) -> PortInfo {
        PortInfo {
            default: Some(json!(default)),
            min: Some(min),
            max: Some(max),
            curve: Some("linear".to_string()),
            ..port(name, kind)
        }
    }

    fn boundary(inputs: Vec<PortInfo>) -> Boundary {
        Boundary {
            instrument: "toy".to_string(),
            inputs,
            outputs: Vec::new(),
            dark_inputs: Vec::new(),
            dark_outputs: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn doc(controls: &str) -> SurfaceDoc {
        SurfaceDoc::parse(&format!(
            r#"{{"surface_version":1,"instrument":"toy","controls":{controls}}}"#
        ))
        .expect("a valid surface doc")
    }

    fn args(message: Option<ControlSendMessage>) -> Vec<serde_json::Value> {
        message.expect("a message").args
    }

    #[test]
    fn a_generated_panel_has_one_control_per_playable_input() {
        let panel = Panel::generated(&boundary(vec![
            PortInfo {
                unit: "Hz".to_string(),
                curve: Some("exponential".to_string()),
                ..ranged("cutoff", "value", 800.0, 20.0, 20000.0)
            },
            ranged("steps", "int", 16.0, 1.0, 16.0),
            ranged("position", "signal", 0.0, 0.0, 1.0),
            PortInfo {
                default: Some(json!("Tukey")),
                variants: vec!["Hann".to_string(), "Tukey".to_string()],
                ..port("window", "enum")
            },
            port("notes", "message"),
            port("audio", "signal"),
            PortInfo {
                channel: Some(0),
                ..port("mic", "signal")
            },
            port("chords", "harmony"),
        ]));

        let kinds: Vec<(&str, &Kind)> = panel
            .controls
            .iter()
            .map(|c| (c.bind.as_str(), &c.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (
                    "cutoff",
                    &Kind::Value {
                        shape: Shape::Fader,
                        min: Some(20.0),
                        max: Some(20000.0),
                        rest: 800.0,
                        integer: false,
                        exponential: true,
                        unit: "Hz".to_string(),
                    }
                ),
                (
                    "steps",
                    &Kind::Value {
                        shape: Shape::Fader,
                        min: Some(1.0),
                        max: Some(16.0),
                        rest: 16.0,
                        integer: true,
                        exponential: false,
                        unit: String::new(),
                    }
                ),
                (
                    "position",
                    &Kind::Value {
                        shape: Shape::Fader,
                        min: Some(0.0),
                        max: Some(1.0),
                        rest: 0.0,
                        integer: false,
                        exponential: false,
                        unit: String::new(),
                    }
                ),
                (
                    "window",
                    &Kind::Choice {
                        variants: vec!["Hann".to_string(), "Tukey".to_string()],
                        rest: 1,
                    }
                ),
                ("notes", &Kind::Keyboard),
            ]
        );
        assert_eq!(panel.warnings.len(), 3, "{:?}", panel.warnings);
        for skipped in ["audio", "mic", "chords"] {
            assert!(
                panel.warnings.iter().any(|w| w.contains(skipped)),
                "{skipped}: {:?}",
                panel.warnings
            );
        }
    }

    #[test]
    fn default_labels_uppercase_each_word_and_nothing_else() {
        assert_eq!(default_label("kick_step1"), "Kick Step1");
        assert_eq!(default_label("mix2out"), "Mix2out");
        assert_eq!(default_label("tempo"), "Tempo");
    }

    #[test]
    fn a_surface_control_takes_its_presentation_from_the_doc_and_its_range_from_the_input() {
        let panel = Panel::from_surface(
            &doc(
                r#"[{"bind":"spray","label":"Scatter","widget":"radial","group":"grain","max":500},
                    {"bind":"spray"}]"#,
            ),
            &boundary(vec![PortInfo {
                unit: "ms".to_string(),
                ..ranged("spray", "value", 40.0, 0.0, 1000.0)
            }]),
        );
        assert_eq!(panel.warnings, Vec::<String>::new());
        assert_eq!(
            panel.controls,
            vec![
                Control {
                    bind: "spray".to_string(),
                    label: "Scatter".to_string(),
                    group: Some("grain".to_string()),
                    kind: Kind::Value {
                        shape: Shape::Radial,
                        min: Some(0.0),
                        max: Some(500.0),
                        rest: 40.0,
                        integer: false,
                        exponential: false,
                        unit: "ms".to_string(),
                    },
                },
                Control {
                    bind: "spray".to_string(),
                    label: "Spray".to_string(),
                    group: None,
                    kind: Kind::Value {
                        shape: Shape::Fader,
                        min: Some(0.0),
                        max: Some(1000.0),
                        rest: 40.0,
                        integer: false,
                        exponential: false,
                        unit: "ms".to_string(),
                    },
                },
            ]
        );
    }

    #[test]
    fn a_narrowed_range_cannot_widen_the_input_and_the_rest_value_stays_inside_it() {
        let panel = Panel::from_surface(
            &doc(r#"[{"bind":"level","min":-2,"max":0.25}]"#),
            &boundary(vec![ranged("level", "value", 0.8, 0.0, 1.0)]),
        );
        let Kind::Value { min, max, rest, .. } = &panel.controls[0].kind else {
            panic!("a value control");
        };
        assert_eq!((*min, *max, *rest), (Some(0.0), Some(0.25), 0.25));
        assert_eq!(panel.warnings.len(), 1, "{:?}", panel.warnings);
        assert!(panel.warnings[0].contains("min -2"), "{:?}", panel.warnings);
    }

    #[test]
    fn a_narrowed_range_with_min_above_max_is_ignored_with_a_warning() {
        let panel = Panel::from_surface(
            &doc(r#"[{"bind":"level","min":0.8,"max":0.2}]"#),
            &boundary(vec![ranged("level", "value", 0.5, 0.0, 1.0)]),
        );
        let Kind::Value { min, max, .. } = &panel.controls[0].kind else {
            panic!("a value control");
        };
        assert_eq!((*min, *max), (Some(0.0), Some(1.0)));
        assert_eq!(panel.warnings.len(), 1, "{:?}", panel.warnings);
        assert!(
            panel.warnings[0].contains("above max"),
            "{:?}",
            panel.warnings
        );
    }

    #[test]
    fn unresolvable_surface_controls_are_skipped_with_a_warning_each() {
        let panel = Panel::from_surface(
            &SurfaceDoc::parse(
                r#"{"surface_version":1,"instrument":"other","controls":[
                    {"bind":"gone"},
                    {"bind":"notes"},
                    {"bind":"level","widget":"xy-pad"},
                    {"bind":"notes","widget":"note-toggle"},
                    {"bind":"notes","widget":"chord-button"},
                    {"bind":"level"}]}"#,
            )
            .expect("a valid surface doc"),
            &boundary(vec![
                ranged("level", "value", 0.5, 0.0, 1.0),
                port("notes", "message"),
            ]),
        );
        assert_eq!(panel.controls.len(), 1);
        assert_eq!(panel.controls[0].bind, "level");
        assert_eq!(panel.warnings.len(), 6, "{:?}", panel.warnings);
        for (warning, names) in panel.warnings.iter().zip([
            "`other`",
            "`gone`",
            "no `widget`",
            "`xy-pad`",
            "no `note`",
            "no `degree`",
        ]) {
            assert!(warning.contains(names), "{names}: {warning}");
        }
    }

    #[test]
    fn the_button_widgets_resolve_with_their_payloads() {
        let panel = Panel::from_surface(
            &doc(r#"[{"bind":"step","widget":"param-toggle"},
                    {"bind":"notes","widget":"note-toggle","note":60,"velocity":0.5},
                    {"bind":"notes","widget":"chord-button","degree":4},
                    {"bind":"notes","widget":"keyboard"}]"#),
            &boundary(vec![
                ranged("step", "value", 1.0, 0.0, 1.0),
                port("notes", "message"),
            ]),
        );
        assert_eq!(panel.warnings, Vec::<String>::new());
        let kinds: Vec<&Kind> = panel.controls.iter().map(|c| &c.kind).collect();
        assert_eq!(
            kinds,
            vec![
                &Kind::Toggle { rest: true },
                &Kind::NoteToggle {
                    note: 60.0,
                    velocity: 0.5
                },
                &Kind::ChordButton { degree: 4 },
                &Kind::Keyboard,
            ]
        );
    }

    #[test]
    fn groups_take_rows_of_their_own_and_the_rest_flow_by_cols() {
        let grouped = |group: Option<&str>| Control {
            bind: "x".to_string(),
            label: "X".to_string(),
            group: group.map(str::to_string),
            kind: Kind::Keyboard,
        };
        let mut controls = vec![grouped(None); 3];
        controls.extend(vec![grouped(Some("kick")); 18]);
        controls.extend(vec![grouped(Some("snare")); 2]);
        controls.push(grouped(None));
        let panel = Panel {
            controls,
            cols: 2,
            warnings: Vec::new(),
        };
        let widths: Vec<usize> = panel.rows().iter().map(Vec::len).collect();
        assert_eq!(widths, vec![2, 1, 16, 2, 2, 1]);
        let flat: Vec<usize> = panel.rows().into_iter().flatten().collect();
        assert_eq!(flat, (0..24).collect::<Vec<usize>>());
    }

    #[test]
    fn every_message_goes_to_the_inputs_in_port() {
        let control = Control {
            bind: "cutoff".to_string(),
            label: "Cutoff".to_string(),
            group: None,
            kind: Kind::Keyboard,
        };
        assert_eq!(control.address(), "/cutoff/in");
    }

    #[test]
    fn a_value_is_sent_as_a_float_clamped_to_the_controls_range() {
        let value = |integer| Control {
            bind: "v".to_string(),
            label: "V".to_string(),
            group: None,
            kind: Kind::Value {
                shape: Shape::Fader,
                min: Some(1.0),
                max: Some(16.0),
                rest: 4.0,
                integer,
                exponential: false,
                unit: String::new(),
            },
        };
        assert_eq!(args(value(false).message(Gesture::Set(2.4))), [json!(2.4)]);
        assert_eq!(
            args(value(false).message(Gesture::Set(99.0))),
            [json!(16.0)]
        );
        // A whole value still travels as a float: an integer atom would be a scale degree on a
        // note input, and a float is what every input kind accepts.
        let whole = args(value(true).message(Gesture::Set(2.6)));
        assert_eq!(whole, [json!(3.0)]);
        assert!(whole[0].is_f64());
        assert!(value(false).message(Gesture::Switch(true)).is_none());
    }

    #[test]
    fn switches_and_choices_send_their_payloads() {
        let control = |kind| Control {
            bind: "b".to_string(),
            label: "B".to_string(),
            group: None,
            kind,
        };
        let toggle = control(Kind::Toggle { rest: false });
        assert_eq!(args(toggle.message(Gesture::Switch(true))), [json!(1.0)]);
        assert_eq!(args(toggle.message(Gesture::Switch(false))), [json!(0.0)]);

        let choice = control(Kind::Choice {
            variants: vec!["Hann".to_string(), "Rect".to_string()],
            rest: 0,
        });
        assert_eq!(args(choice.message(Gesture::Pick(1))), [json!("Rect")]);
        assert!(choice.message(Gesture::Pick(2)).is_none());
    }

    #[test]
    fn a_note_is_pitch_then_velocity_and_velocity_zero_releases_it() {
        let control = |kind| Control {
            bind: "notes".to_string(),
            label: "Notes".to_string(),
            group: None,
            kind,
        };
        let keyboard = control(Kind::Keyboard);
        let down = args(keyboard.message(Gesture::Key {
            midi: 69,
            down: true,
        }));
        assert_eq!(down, [json!(69.0), json!(1.0)]);
        assert!(down[0].is_f64(), "an absolute pitch is a float atom");
        let up = args(keyboard.message(Gesture::Key {
            midi: 69,
            down: false,
        }));
        assert_eq!(up, [json!(69.0), json!(0.0)]);

        let held = control(Kind::NoteToggle {
            note: 60.0,
            velocity: 0.5,
        });
        assert_eq!(
            args(held.message(Gesture::Switch(true))),
            [json!(60.0), json!(0.5)]
        );
        assert_eq!(
            args(held.message(Gesture::Switch(false))),
            [json!(60.0), json!(0.0)]
        );

        let chord = control(Kind::ChordButton { degree: 4 });
        let pressed = args(chord.message(Gesture::Switch(true)));
        assert_eq!(pressed, [json!(4), json!(1.0)]);
        assert!(pressed[0].is_i64(), "a scale degree is an integer atom");
        assert_eq!(
            args(chord.message(Gesture::Switch(false))),
            [json!(4), json!(0.0)]
        );
    }

    #[test]
    fn the_unit_mapping_follows_the_curve_and_inverts() {
        assert_eq!(to_unit(5.0, 0.0, 10.0, false), 0.5);
        assert_eq!(from_unit(0.5, 0.0, 10.0, false), 5.0);
        let mid = from_unit(0.5, 20.0, 20000.0, true);
        assert!((mid - 632.455).abs() < 0.01, "{mid}");
        assert!((to_unit(mid, 20.0, 20000.0, true) - 0.5).abs() < 1e-9);
        // An exponential curve from zero has no logarithm to follow.
        assert_eq!(from_unit(0.5, 0.0, 10.0, true), 5.0);
        assert_eq!(to_unit(3.0, 4.0, 4.0, false), 0.0);
    }

    #[test]
    fn the_keyboard_lays_black_keys_over_the_gaps_they_sit_above() {
        let keys = keys(60, 8);
        let midi: Vec<u8> = keys.iter().map(|k| k.midi).collect();
        assert_eq!(midi, (60..=72).collect::<Vec<u8>>());
        assert_eq!(keys.iter().filter(|k| !k.black).count(), 8);

        assert_eq!(key_at(&keys, 0.5, 0.9), Some(60));
        assert_eq!(key_at(&keys, 0.9, 0.9), Some(60));
        assert_eq!(key_at(&keys, 0.9, 0.2), Some(61));
        assert_eq!(key_at(&keys, 1.1, 0.2), Some(61));
        assert_eq!(key_at(&keys, 1.5, 0.2), Some(62));
        // E to F has no black key between.
        assert_eq!(key_at(&keys, 3.0, 0.2), Some(65));
        assert_eq!(key_at(&keys, 7.5, 0.9), Some(72));
        assert_eq!(key_at(&keys, 8.0, 0.9), None);
        assert_eq!(key_at(&keys, -0.1, 0.9), None);
    }
}
