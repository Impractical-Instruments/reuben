//! The MIDI map as the table shows it: one row per control of the open instrument's map, what
//! checking it against the instrument found, and the edit each of the table's gestures asks for.

use reuben_api::authoring::{
    Boundary, MoveInstrumentSurfaceControl, RemoveInstrumentSurfaceControl,
    SetInstrumentSurfaceControl, SurfaceView,
};
use reuben_api::surface::{check_midi_control, SurfaceControl, SurfaceKind};
use reuben_api::tools::names;

use crate::edit::Verb;

/// The CC route a numeric input is given when its row is added, or rebound to it.
pub const NEW_PATTERN: &str = "/midi/1/cc/1";

/// The routes a control is given when its own pattern does not fit its input, tried in order
/// against the engine's check: notes for a message input, then a CC for a bounded numeric one.
const ROUTES: [&str; 2] = ["/midi/1/note", NEW_PATTERN];

/// What checking a control against the instrument's interface inputs found.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Ok,
    /// Used, with what was adjusted to use it.
    Warned(Vec<String>),
    /// Ignored when the map is played, and why.
    Skipped(String),
}

/// One control of the map.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Its position in the map's `controls`: what an edit's `index` names.
    pub index: usize,
    pub bind: String,
    pub pattern: String,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub status: Status,
}

/// The open instrument's MIDI map, read through `describe_instrument_surface`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MidiMapModel {
    /// What the server answered; `None` when it could not be asked.
    pub view: Option<SurfaceView>,
    pub rows: Vec<Row>,
    /// The instrument's interface inputs, which a row can bind.
    pub inputs: Vec<String>,
    /// Why there are no rows to show: the map does not parse, or could not be read.
    pub error: Option<String>,
    /// The instrument's interface, which a route is checked against before it is proposed.
    pub boundary: Option<Boundary>,
}

impl MidiMapModel {
    pub fn from_view(view: SurfaceView, boundary: &Boundary) -> Self {
        let rows = match view.error {
            // Whatever a doc that does not parse holds is not the map's to edit.
            Some(_) => Vec::new(),
            None => (view.controls.iter().enumerate())
                .map(|(index, checked)| Row {
                    index,
                    bind: checked.control.bind.clone(),
                    pattern: checked.control.midi.clone().unwrap_or_default(),
                    min: checked.control.min,
                    max: checked.control.max,
                    status: match (&checked.skipped, checked.warnings.is_empty()) {
                        (Some(why), _) => Status::Skipped(why.clone()),
                        (None, true) => Status::Ok,
                        (None, false) => Status::Warned(checked.warnings.clone()),
                    },
                })
                .collect(),
        };
        Self {
            rows,
            inputs: boundary.inputs.iter().map(|i| i.name.clone()).collect(),
            error: view.error.clone(),
            view: Some(view),
            boundary: Some(boundary.clone()),
        }
    }

    /// A map that could not be read, saying `why`.
    pub fn unread(why: String) -> Self {
        Self {
            error: Some(why),
            ..Self::default()
        }
    }

    /// The control at `index` as the map holds it, so an edit of one field keeps the others.
    pub fn control(&self, index: usize) -> Option<&SurfaceControl> {
        let view = self.view.as_ref()?;
        view.controls.get(index).map(|checked| &checked.control)
    }

    /// `control`, kept when the engine's check takes it, else given the first of [`ROUTES`] the
    /// check takes for its input. `Err` is why the check refuses it: as it is, or, for a control
    /// with no pattern yet, on the last route tried.
    fn fitted(&self, control: SurfaceControl) -> Result<SurfaceControl, String> {
        let boundary = (self.boundary.as_ref()).ok_or("the instrument's inputs are not known")?;
        let mut why = match check_midi_control(&control, boundary) {
            Ok(_) => return Ok(control),
            Err(why) => why,
        };
        for route in ROUTES {
            let routed = SurfaceControl {
                midi: Some(route.to_string()),
                ..control.clone()
            };
            match check_midi_control(&routed, boundary) {
                Ok(_) => return Ok(routed),
                Err(refused) if control.midi.is_none() => why = refused,
                Err(_) => {}
            }
        }
        Err(why)
    }

    /// The control `+ add` appends: the first interface input a route fits, on that route. `Err`
    /// says why no input takes one, so nothing is asked that the server would refuse.
    pub fn added(&self) -> Result<Action, String> {
        let mut refused = Vec::new();
        for bind in &self.inputs {
            let control = SurfaceControl {
                bind: bind.clone(),
                ..SurfaceControl::default()
            };
            match self.fitted(control) {
                Ok(control) => {
                    return Ok(Action::Set {
                        index: None,
                        control,
                    })
                }
                Err(why) => refused.push(why),
            }
        }
        Err(match refused.is_empty() {
            true => "The instrument has no interface input to map.".to_string(),
            false => format!(
                "No interface input takes a MIDI route: {}.",
                refused.join("; ")
            ),
        })
    }

    /// The edit binding the row at `index` to `bind`. Its pattern is kept where the new input takes
    /// it and replaced, in the same edit, with a route that fits where it does not; a row no route
    /// fits is sent as it is, for the server to say why.
    pub fn rebound(&self, index: usize, bind: &str) -> Option<Action> {
        let control = SurfaceControl {
            bind: bind.to_string(),
            ..self.control(index)?.clone()
        };
        let control = self.fitted(control.clone()).unwrap_or(control);
        Some(Action::Set {
            index: Some(index),
            control,
        })
    }
}

/// One edit of the map that the table asks for.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Write `control` at `index`, or append it.
    Set {
        index: Option<usize>,
        control: SurfaceControl,
    },
    Remove(usize),
    Move {
        from: usize,
        to: usize,
    },
}

impl Action {
    /// The row the edit is of; `None` for an append.
    pub fn row(&self) -> Option<usize> {
        match self {
            Self::Set { index, .. } => *index,
            Self::Remove(index) | Self::Move { from: index, .. } => Some(*index),
        }
    }
}

/// Move the row at `index` up one; `None` for the first row.
pub fn up(index: usize) -> Option<Action> {
    let to = index.checked_sub(1)?;
    Some(Action::Move { from: index, to })
}

/// Move the row at `index` of `len` down one; `None` for the last row.
pub fn down(index: usize, len: usize) -> Option<Action> {
    let to = index + 1;
    (to < len).then_some(Action::Move { from: index, to })
}

/// The verb that makes `action` on the MIDI map of the instrument the server names `top`, guarded
/// by the `expect` it is handed.
pub fn verb(top: &str, action: &Action) -> impl FnOnce(Option<String>) -> Verb {
    let source = top.to_string();
    let action = action.clone();
    move |expect| match action {
        Action::Set { index, control } => Verb::of(
            names::SET_INSTRUMENT_SURFACE_CONTROL,
            &SetInstrumentSurfaceControl {
                source,
                surface: SurfaceKind::Midi,
                index,
                control,
                expect,
            },
        ),
        Action::Remove(index) => Verb::of(
            names::REMOVE_INSTRUMENT_SURFACE_CONTROL,
            &RemoveInstrumentSurfaceControl {
                source,
                surface: SurfaceKind::Midi,
                index,
                expect,
            },
        ),
        Action::Move { from, to } => Verb::of(
            names::MOVE_INSTRUMENT_SURFACE_CONTROL,
            &MoveInstrumentSurfaceControl {
                source,
                surface: SurfaceKind::Midi,
                from,
                to,
                expect,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn boundary() -> Boundary {
        serde_json::from_value(json!({
            "instrument": "synth",
            "inputs": [
                {"name": "notes", "kind": "message"},
                {"name": "cutoff", "kind": "value", "min": 20.0, "max": 20000.0},
                {"name": "drive", "kind": "value"}
            ],
            "outputs": []
        }))
        .expect("a boundary")
    }

    fn view(controls: serde_json::Value) -> SurfaceView {
        serde_json::from_value(json!({
            "source": "/lib/surfaces/synth.midi.json",
            "exists": true,
            "hash": "h1",
            "instrument": "synth",
            "controls": controls
        }))
        .expect("a surface view")
    }

    #[test]
    fn rows_carry_each_controls_status() {
        let model = MidiMapModel::from_view(
            view(json!([
                {"control": {"bind": "notes", "midi": "/midi/1/note"}},
                {"control": {"bind": "cutoff", "midi": "/midi/{1,2}/cc/74", "min": 10.0,
                             "max": 8000.0},
                 "warnings": ["min 10 is clamped into cutoff's range"]},
                {"control": {"bind": "drive", "midi": "/midi/1/cc/1"},
                 "skipped": "no range to scale into"}
            ])),
            &boundary(),
        );
        assert_eq!(model.error, None);
        assert_eq!(model.inputs, ["notes", "cutoff", "drive"]);
        assert_eq!(
            model.rows,
            [
                Row {
                    index: 0,
                    bind: "notes".to_string(),
                    pattern: "/midi/1/note".to_string(),
                    min: None,
                    max: None,
                    status: Status::Ok,
                },
                Row {
                    index: 1,
                    bind: "cutoff".to_string(),
                    pattern: "/midi/{1,2}/cc/74".to_string(),
                    min: Some(10.0),
                    max: Some(8000.0),
                    status: Status::Warned(vec![
                        "min 10 is clamped into cutoff's range".to_string()
                    ]),
                },
                Row {
                    index: 2,
                    bind: "drive".to_string(),
                    pattern: "/midi/1/cc/1".to_string(),
                    min: None,
                    max: None,
                    status: Status::Skipped("no range to scale into".to_string()),
                },
            ]
        );
    }

    #[test]
    fn an_unparsable_map_shows_its_error_and_no_rows() {
        let broken = SurfaceView {
            hash: None,
            error: Some("is not a surface doc: expected value at line 1".to_string()),
            ..view(json!([]))
        };
        let model = MidiMapModel::from_view(broken, &boundary());
        assert_eq!(
            model.error.as_deref(),
            Some("is not a surface doc: expected value at line 1")
        );
        assert_eq!(model.rows, []);
        assert_eq!(model.inputs, ["notes", "cutoff", "drive"]);
    }

    #[test]
    fn verb_for_set_names_the_tool_and_passes_expect() {
        let control = SurfaceControl {
            bind: "cutoff".to_string(),
            midi: Some("/midi/1/cc/74".to_string()),
            ..Default::default()
        };
        let action = Action::Set {
            index: Some(1),
            control,
        };
        let set = verb("/lib/synth.json", &action)(Some("h1".to_string()));
        assert_eq!(set.tool, names::SET_INSTRUMENT_SURFACE_CONTROL);
        assert_eq!(
            set.args,
            json!({
                "source": "/lib/synth.json",
                "surface": "midi",
                "index": 1,
                "control": {"bind": "cutoff", "midi": "/midi/1/cc/74"},
                "expect": "h1"
            })
        );

        let remove = verb("/lib/synth.json", &Action::Remove(0))(None);
        assert_eq!(remove.tool, names::REMOVE_INSTRUMENT_SURFACE_CONTROL);
        assert_eq!(remove.args["expect"], serde_json::Value::Null);
    }

    /// A boundary with only `inputs`, each a `{"name", "kind", …}` port.
    fn inputs(inputs: serde_json::Value) -> Boundary {
        serde_json::from_value(json!({"instrument": "synth", "inputs": inputs, "outputs": []}))
            .expect("a boundary")
    }

    fn missing() -> SurfaceView {
        SurfaceView {
            exists: false,
            hash: None,
            ..view(json!([]))
        }
    }

    fn route(bind: &str, pattern: &str) -> SurfaceControl {
        SurfaceControl {
            bind: bind.to_string(),
            midi: Some(pattern.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn add_on_a_message_only_instrument_proposes_a_note_route() {
        let notes = inputs(json!([{"name": "notes", "kind": "message"}]));
        let model = MidiMapModel::from_view(missing(), &notes);
        assert_eq!(
            model.added(),
            Ok(Action::Set {
                index: None,
                control: route("notes", "/midi/1/note"),
            })
        );
    }

    #[test]
    fn add_proposes_the_first_input_a_route_fits() {
        // `drive` is unbounded, so no CC can scale into it; `cutoff` is bounded.
        let model = MidiMapModel::from_view(missing(), &boundary());
        let Ok(Action::Set { control, .. }) = model.added() else {
            panic!("{:?}", model.added());
        };
        assert_eq!(control, route("notes", "/midi/1/note"));

        let numeric = inputs(json!([
            {"name": "drive", "kind": "value"},
            {"name": "cutoff", "kind": "value", "min": 20.0, "max": 20000.0}
        ]));
        let model = MidiMapModel::from_view(missing(), &numeric);
        assert_eq!(
            model.added(),
            Ok(Action::Set {
                index: None,
                control: route("cutoff", NEW_PATTERN),
            })
        );
    }

    #[test]
    fn add_with_no_input_a_route_fits_says_why_and_asks_nothing() {
        let unfit = inputs(json!([
            {"name": "drive", "kind": "value"},
            {"name": "label", "kind": "string"}
        ]));
        let why = MidiMapModel::from_view(missing(), &unfit)
            .added()
            .expect_err("nothing to add");
        assert!(why.contains("drive") && why.contains("label"), "{why}");
        assert!(why.contains("no range to scale into"), "{why}");

        let none = inputs(json!([]));
        assert!(MidiMapModel::from_view(missing(), &none).added().is_err());
    }

    #[test]
    fn rebinding_a_cc_row_to_a_message_input_is_one_set_with_a_note_pattern() {
        let both = inputs(json!([
            {"name": "notes", "kind": "message"},
            {"name": "cutoff", "kind": "value", "min": 20.0, "max": 20000.0},
            {"name": "level", "kind": "value", "min": 0.0, "max": 1.0}
        ]));
        let model = MidiMapModel::from_view(
            view(json!([{"control": {"bind": "cutoff", "midi": "/midi/{1,2}/cc/74"}}])),
            &both,
        );
        assert_eq!(
            model.rebound(0, "notes"),
            Some(Action::Set {
                index: Some(0),
                control: route("notes", "/midi/1/note"),
            })
        );
        // A pattern the new input takes is kept.
        assert_eq!(
            model.rebound(0, "level"),
            Some(Action::Set {
                index: Some(0),
                control: route("level", "/midi/{1,2}/cc/74"),
            })
        );
    }

    #[test]
    fn move_up_from_zero_is_no_action() {
        assert_eq!(up(0), None);
        assert_eq!(up(2), Some(Action::Move { from: 2, to: 1 }));
        assert_eq!(down(2, 3), None);
        assert_eq!(down(1, 3), Some(Action::Move { from: 1, to: 2 }));
    }
}
