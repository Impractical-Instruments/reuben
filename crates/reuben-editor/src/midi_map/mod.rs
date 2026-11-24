//! The MIDI map table: the open instrument's MIDI map, one row per control, with what checking each
//! against the instrument's interface inputs found.
//!
//! The table reads the map through `describe_instrument_surface` and edits it through the surface
//! verbs, on the editor's one write path ([`edit::write_to`](crate::edit::write_to)), so each
//! edit is an undo step like any other. It is read again whenever the map, or the instrument whose
//! inputs it binds, may have changed.

mod model;
mod view;

use eframe::egui;
use reuben_api::authoring::{
    Boundary, DescribeBoundary, DescribeInstrumentSurface, EditResult, Refusal, SurfaceView,
};
use reuben_api::engine::{call_tool, Channel, Event};
use reuben_api::surface::SurfaceKind;
use reuben_api::tools::names;

pub use self::model::{verb, Action};
use self::model::{MidiMapModel, Row};
use self::view::{Asked, Draft};

/// The open instrument's MIDI map, and what the table's fields hold.
#[derive(Debug, Default)]
pub struct MidiMap {
    model: MidiMapModel,
    drafts: Vec<Draft>,
    /// Why the last edit was not written, and the row it was of (`None` for an append): shown
    /// until the next edit.
    refused: Option<(Option<usize>, String)>,
    /// The map, or the inputs it binds, may have changed since it was read.
    stale: bool,
}

/// Read the MIDI map of the instrument the server names `top`, which it has open.
fn load(channel: &Channel, top: &str) -> MidiMapModel {
    let args = DescribeBoundary {
        source: top.to_string(),
    };
    let boundary = call_tool(names::DESCRIBE_BOUNDARY, &args, channel)
        .map_err(|refusal| refusal.message)
        .and_then(|answer| {
            serde_json::from_value::<Boundary>(answer.output)
                .map_err(|e| format!("the server's boundary could not be read: {e}"))
        });
    let boundary = match boundary {
        Ok(boundary) => boundary,
        Err(e) => {
            return MidiMapModel::unread(format!("No MIDI map: the instrument did not load. {e}"))
        }
    };
    let args = DescribeInstrumentSurface {
        source: top.to_string(),
        surface: SurfaceKind::Midi,
    };
    let view = call_tool(names::DESCRIBE_INSTRUMENT_SURFACE, &args, channel)
        .map_err(|refusal| refusal.message)
        .and_then(|answer| {
            serde_json::from_value::<SurfaceView>(answer.output)
                .map_err(|e| format!("the server's answer could not be read: {e}"))
        });
    match view {
        Ok(view) => MidiMapModel::from_view(view, &boundary),
        Err(e) => MidiMapModel::unread(format!("The MIDI map could not be read: {e}")),
    }
}

/// Why `result` was not written; `None` when it was.
fn not_written(result: &Result<EditResult, Refusal>) -> Option<String> {
    let edit = match result {
        Ok(edit) if edit.written => return None,
        Ok(edit) => edit,
        Err(refusal) => return Some(refusal.message.clone()),
    };
    let said: Vec<&str> = (edit.report.errors.iter())
        .chain(&edit.pre_existing)
        .map(|error| error.message.as_str())
        .chain(edit.notes.iter().map(String::as_str))
        .collect();
    Some(match said.is_empty() {
        true => "the edit was not written".to_string(),
        false => said.join("; "),
    })
}

impl MidiMap {
    /// The MIDI map of the open instrument the server names `top`, read through `channel`.
    pub fn open(top: Option<&str>, channel: &dyn Fn() -> Option<Channel>) -> Self {
        let mut map = Self::default();
        map.reload(top, channel);
        map
    }

    /// Read the map again. A row whose control the map still holds as it did keeps what is typed
    /// in its fields, so a refused edit, or one not yet committed, is not lost.
    pub fn reload(&mut self, top: Option<&str>, channel: &dyn Fn() -> Option<Channel>) {
        let model = match top {
            None => MidiMapModel::default(),
            Some(top) => match channel() {
                Some(channel) => load(&channel, top),
                None => MidiMapModel::unread(
                    "No MIDI map: no engine is running, so it cannot be read.".to_string(),
                ),
            },
        };
        let unchanged = |new: &Row| {
            let old = self.model.rows.get(new.index)?;
            let same = (old.bind == new.bind)
                && (old.pattern == new.pattern)
                && (old.min == new.min)
                && (old.max == new.max);
            same.then(|| self.drafts.get(new.index).cloned()).flatten()
        };
        self.drafts = (model.rows.iter())
            .map(|row| unchanged(row).unwrap_or_else(|| Draft::of(row)))
            .collect();
        self.model = model;
    }

    /// Take in one of the engine's events: a MIDI map edit, or a change to what is open, whose
    /// inputs the map binds.
    pub fn hear(&mut self, event: &Event) {
        match event {
            Event::SurfaceChanged {
                surface: SurfaceKind::Midi,
                ..
            }
            | Event::Changed { .. }
            | Event::Opened { .. } => self.stale = true,
            _ => {}
        }
    }

    /// Whether a change was [`hear`](Self::hear)d since the last call, which means the map is to
    /// be [`reload`](Self::reload)ed.
    pub fn take_stale(&mut self) -> bool {
        std::mem::take(&mut self.stale)
    }

    /// The map as the server last answered it: where it is, and the hash to guard an edit with.
    pub fn view(&self) -> Option<&SurfaceView> {
        self.model.view.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn model(&self) -> &MidiMapModel {
        &self.model
    }

    /// Take in what the server answered `action`.
    pub fn answered(&mut self, action: &Action, result: &Result<EditResult, Refusal>) {
        self.refused = not_written(result).map(|why| (action.row(), why));
    }

    /// Draw the table, and return the edit asked of it this frame.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let asked = egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                view::table(ui, &self.model, &mut self.drafts, self.refused.as_ref())
            })
            .inner;
        match asked? {
            Asked::Edit(action) => Some(action),
            Asked::Refuse(at, why) => {
                self.refused = Some((Some(at), why));
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_midi_map_edit_of_the_surface_edits_makes_the_map_stale() {
        let mut map = MidiMap::default();
        map.hear(&Event::SurfaceChanged {
            surface: SurfaceKind::Screen,
            hash: String::new(),
        });
        assert!(!map.take_stale());
        map.hear(&Event::SurfaceChanged {
            surface: SurfaceKind::Midi,
            hash: String::new(),
        });
        assert!(map.take_stale());
        assert!(!map.take_stale(), "taken once");
    }
}
