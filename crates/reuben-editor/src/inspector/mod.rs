//! The inspector: the selected node's inputs, constants and description, or with nothing selected
//! the instrument's name and description.
//!
//! What it shows is the session's read of the top-level document, so it is always the file. Every
//! change it makes is one authoring verb through [`edit::write`], except a number being dragged: its
//! moves go to the engine live, and the document is written once, when it is let go.

mod model;
mod view;

use reuben_api::authoring::{
    set_instrument_constant, set_instrument_description, set_instrument_input, set_instrument_name,
    set_instrument_node_description, Answer, EditResult, Refusal, Resources, SetInstrumentConstant,
    SetInstrumentDescription, SetInstrumentInput, SetInstrumentName, SetInstrumentNodeDescription,
};
use reuben_api::engine::{send_live_controls, Channel, ControlSendMessage, SendLiveControls};
use serde_json::Value;

use self::model::{Holds, Step, Target};
use crate::edit;
use crate::session::Session;

pub use self::view::show;

/// One change to the document.
#[derive(Debug, Clone, PartialEq)]
enum Edit {
    Value { target: Target, value: Value },
    NodeDescription { address: String, text: String },
    Name(String),
    Description(String),
}

impl Edit {
    /// Run this edit's verb on the document `source`. Empty text clears a description.
    fn run(
        self,
        source: &str,
        store: &dyn Resources,
        expect: Option<String>,
    ) -> Result<Answer<EditResult>, Refusal> {
        let source = source.to_string();
        let cleared = |text: String| (!text.trim().is_empty()).then_some(text);
        match self {
            Edit::Value { target, value } if target.constant => set_instrument_constant(
                &SetInstrumentConstant {
                    source,
                    address: target.address,
                    name: target.name,
                    value,
                    expect,
                },
                store,
            ),
            Edit::Value { target, value } => set_instrument_input(
                &SetInstrumentInput {
                    source,
                    address: target.address,
                    input: target.name,
                    value,
                    expect,
                },
                store,
            ),
            Edit::NodeDescription { address, text } => set_instrument_node_description(
                &SetInstrumentNodeDescription {
                    source,
                    address,
                    description: cleared(text),
                    expect,
                },
                store,
            ),
            Edit::Name(name) => set_instrument_name(
                &SetInstrumentName {
                    source,
                    name,
                    expect,
                },
                store,
            ),
            Edit::Description(text) => set_instrument_description(
                &SetInstrumentDescription {
                    source,
                    description: cleared(text),
                    expect,
                },
                store,
            ),
        }
    }
}

/// A text field being typed into, kept until it loses focus.
#[derive(Debug, Clone, PartialEq)]
struct Draft {
    /// Which field: the node's address, or empty for the instrument's own.
    owner: String,
    /// `name` or `description`.
    field: &'static str,
    text: String,
}

/// What the inspector keeps between frames.
#[derive(Debug, Default)]
pub struct Inspector {
    holds: Holds,
    draft: Option<Draft>,
    /// Why the last edit or live send did not land, until the next one does.
    error: Option<String>,
}

impl Inspector {
    /// Do what a number field asked for this frame.
    fn step(&mut self, step: Step, session: &mut Session, channel: &dyn Fn() -> Option<Channel>) {
        match step {
            Step::Live(target, value) => {
                if let Some(message) = target.live(value) {
                    self.live(message, channel);
                }
            }
            Step::Commit(target, value) => {
                let value = target.literal(value);
                self.commit(Edit::Value { target, value }, session, channel);
            }
        }
    }

    /// Send `message` to the running engine, when one is running.
    fn live(&mut self, message: ControlSendMessage, channel: &dyn Fn() -> Option<Channel>) {
        let Some(channel) = channel() else { return };
        let sent = send_live_controls(
            &SendLiveControls {
                messages: vec![message],
            },
            &channel,
        );
        self.error = sent.err().map(|refusal| refusal.message);
    }

    /// Write `edit` to the top-level document.
    fn commit(&mut self, edit: Edit, session: &mut Session, channel: &dyn Fn() -> Option<Channel>) {
        let source = session.tree().top().source.clone();
        let result = edit::write(session, channel, &source, |store, expect| {
            edit.run(&source, store, expect)
        });
        self.error = match result {
            Ok(result) if result.written => None,
            Ok(result) => Some(refused(&result)),
            Err(refusal) => Some(refusal.message),
        };
    }
}

/// Why an edit that ran wrote nothing.
fn refused(result: &EditResult) -> String {
    let said: Vec<String> = (result.report.errors.iter())
        .map(|e| match (&e.node, &e.port) {
            (Some(node), Some(port)) => format!("{node}.{port}: {}", e.message),
            (Some(node), None) => format!("{node}: {}", e.message),
            _ => e.message.clone(),
        })
        .chain(result.notes.iter().cloned())
        .collect();
    if said.is_empty() {
        "the edit was not written".to_string()
    } else {
        said.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use reuben_api::authoring::{InputValue, StructureNode};
    use reuben_native::test_support::within;
    use serde_json::json;

    use super::model::{fields, Control};
    use super::*;
    use crate::session::test_support::Rig;
    use crate::session::Status;

    const HANG_SECS: u64 = 60;

    /// The instrument the tests open: its `/map_cutoff` sets the filter's highest cutoff with a
    /// literal, and its `/voicer` sets a constant.
    const INSTRUMENT: &str = "strum-harp.json";

    fn node(rig: &Rig, address: &str) -> StructureNode {
        let top = rig
            .session
            .tree()
            .top()
            .read
            .as_ref()
            .expect("the top reads");
        (top.nodes.iter())
            .find(|n| n.address == address)
            .unwrap_or_else(|| panic!("{address} is a node"))
            .clone()
    }

    fn target(rig: &Rig, address: &str, name: &str) -> Target {
        fields(&node(rig, address))
            .into_iter()
            .find(|f| f.target.name == name)
            .unwrap_or_else(|| panic!("{address} has {name}"))
            .target
    }

    fn literal(rig: &Rig, address: &str, name: &str) -> Value {
        let node = node(rig, address);
        let held = node
            .inputs
            .iter()
            .find(|i| i.name == name)
            .map(|i| i.value.clone())
            .or_else(|| {
                (node.constants.iter())
                    .find(|c| c.name == name)
                    .map(|c| InputValue::Literal {
                        value: c.value.clone(),
                    })
            });
        match held {
            Some(InputValue::Literal { value }) => value,
            other => panic!("{address}.{name} holds {other:?}"),
        }
    }

    /// Hold `target`, move it through `values`, and let go: the frames a slider sweep makes.
    fn sweep(
        rig: &mut Rig,
        inspector: &mut Inspector,
        target: &Target,
        values: &[f64],
        during: impl Fn(&mut Rig),
    ) {
        let channel = rig.channel_source();
        let document = match fields(&node(rig, &target.address))
            .into_iter()
            .find(|f| &f.target == target)
            .map(|f| f.control)
        {
            Some(Control::Number { value, .. }) => value,
            other => panic!("{target:?} is {other:?}"),
        };
        for &value in values {
            inspector.holds.begin();
            for step in inspector.holds.number(target, document, value, true, true) {
                inspector.step(step, &mut rig.session, &channel);
            }
            assert_eq!(inspector.holds.end(), None);
            assert_eq!(inspector.error, None);
            during(rig);
        }
        let last = *values.last().expect("a sweep moves");
        inspector.holds.begin();
        for step in inspector.holds.number(target, document, last, false, false) {
            inspector.step(step, &mut rig.session, &channel);
        }
        assert_eq!(inspector.holds.end(), None);
    }

    #[test]
    fn a_swept_input_goes_live_without_a_write_and_letting_go_writes_once() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_sweep", INSTRUMENT);
            let mut inspector = Inspector::default();
            let cutoff = target(&rig, "/map_cutoff", "out_max");
            assert!(!cutoff.constant);
            let before = rig.session.tree().top().hash().map(str::to_string);

            let values: Vec<f64> = (0..40).map(|i| 600.0 + 250.0 * f64::from(i)).collect();
            sweep(&mut rig, &mut inspector, &cutoff, &values, |rig| {
                assert_eq!(rig.swaps(), 0, "a held value is never written");
                assert_eq!(
                    rig.session.tree().top().hash(),
                    before.as_deref(),
                    "a held value is never written"
                );
            });
            assert_eq!(inspector.error, None);

            assert_ne!(rig.session.tree().top().hash(), before.as_deref());
            assert_eq!(literal(&rig, "/map_cutoff", "out_max"), json!(10350.0));
            rig.poll_until("the Swap of the write", |rig| rig.swaps() == 1);
            rig.settle();
            assert_eq!(rig.swaps(), 1, "one write, one Swap");
            assert_eq!(rig.status(), &Status::Current);
            assert_eq!(
                Some(rig.playing().as_str()),
                rig.session.tree().top().hash(),
                "the engine plays the released value"
            );
        });
    }

    #[test]
    fn a_held_constant_is_written_once_on_release() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_constant", INSTRUMENT);
            let mut inspector = Inspector::default();
            let voices = target(&rig, "/voicer", "voices");
            assert!(voices.constant && voices.integer, "{voices:?}");

            sweep(&mut rig, &mut inspector, &voices, &[7.0, 5.6, 4.2], |rig| {
                assert_eq!(rig.swaps(), 0);
            });
            assert_eq!(inspector.error, None);
            assert_eq!(literal(&rig, "/voicer", "voices").as_f64(), Some(4.0));
            rig.poll_until("the Swap of the write", |rig| rig.swaps() == 1);
            rig.settle();
            assert_eq!(rig.swaps(), 1);
        });
    }

    #[test]
    fn a_choice_and_the_descriptions_and_the_name_each_write_through_their_verb() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_text", INSTRUMENT);
            let mut inspector = Inspector::default();
            let channel = rig.channel_source();
            let curve = fields(&node(&rig, "/map_cutoff"))
                .into_iter()
                .find(|f| f.target.name == "curve")
                .expect("map_f32_signal has a curve");
            let Control::Choice { variants, value } = &curve.control else {
                panic!("{curve:?}");
            };
            let other = variants
                .iter()
                .find(|v| *v != value)
                .expect("a second curve");

            for edit in [
                Edit::Value {
                    target: curve.target.clone(),
                    value: json!(other),
                },
                Edit::NodeDescription {
                    address: "/map_cutoff".into(),
                    text: "the brightest the filter opens".into(),
                },
                Edit::Name("strum-harp-edited".into()),
                Edit::Description("edited in the inspector".into()),
            ] {
                inspector.commit(edit.clone(), &mut rig.session, &channel);
                assert_eq!(inspector.error, None, "{edit:?}");
            }

            let top = rig.session.tree().top().read.clone().expect("reads");
            assert_eq!(literal(&rig, "/map_cutoff", "curve"), json!(other));
            assert_eq!(
                node(&rig, "/map_cutoff").description.as_deref(),
                Some("the brightest the filter opens")
            );
            assert_eq!(top.instrument, "strum-harp-edited");
            assert_eq!(top.description.as_deref(), Some("edited in the inspector"));

            inspector.commit(
                Edit::NodeDescription {
                    address: "/map_cutoff".into(),
                    text: "  ".into(),
                },
                &mut rig.session,
                &channel,
            );
            assert_eq!(inspector.error, None);
            assert_eq!(node(&rig, "/map_cutoff").description, None);
        });
    }

    #[test]
    fn a_refused_value_is_reported_and_nothing_is_written() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_refused", INSTRUMENT);
            let mut inspector = Inspector::default();
            let channel = rig.channel_source();
            let before = rig.session.tree().clone();
            let mut mode = target(&rig, "/map_cutoff", "curve");
            mode.integer = false;
            inspector.commit(
                Edit::Value {
                    target: mode,
                    value: json!("NoSuchCurve"),
                },
                &mut rig.session,
                &channel,
            );
            assert!(inspector.error.is_some());
            assert_eq!(rig.session.tree(), &before);
            rig.settle();
            assert_eq!(rig.swaps(), 0);
        });
    }
}
