//! The MIDI map as the table shows it: one row per control of the open instrument's map, what
//! checking it against the instrument found, and the edit each of the table's gestures asks for.

use reuben_api::authoring::{
    Boundary, MoveInstrumentSurfaceControl, RemoveInstrumentSurfaceControl,
    SetInstrumentSurfaceControl, SurfaceView,
};
use reuben_api::surface::{SurfaceControl, SurfaceKind};
use reuben_api::tools::names;

use crate::edit::Verb;

/// The pattern a row added with `+ add` starts with.
pub const NEW_PATTERN: &str = "/midi/1/cc/1";

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

    /// The control `+ add` appends: the first interface input, on [`NEW_PATTERN`]. `None` for an
    /// instrument with no input to bind.
    pub fn added(&self) -> Option<Action> {
        let bind = self.inputs.first()?.clone();
        Some(Action::Set {
            index: None,
            control: SurfaceControl {
                bind,
                midi: Some(NEW_PATTERN.to_string()),
                ..SurfaceControl::default()
            },
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

    #[test]
    fn move_up_from_zero_is_no_action() {
        assert_eq!(up(0), None);
        assert_eq!(up(2), Some(Action::Move { from: 2, to: 1 }));
        assert_eq!(down(2, 3), None);
        assert_eq!(down(1, 3), Some(Action::Move { from: 1, to: 2 }));
    }
}
