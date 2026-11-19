//! The inspector: the selected node's inputs, constants and description, the selected interface
//! pipe's value and metadata, or with nothing selected the instrument's name, description,
//! interface pipes and resources table.
//!
//! What it shows is the session's read of the document it views, so it is always the file. Every
//! change it makes is one authoring verb through [`edit::write`], except a number being dragged in
//! the top-level document: its moves go to the engine live, and the document is written once, when
//! it is let go. A voice's nodes have no address the engine can be sent to, and a patch's
//! have one per node using the patch, so a number dragged in either is only written, on release.

mod interface;
mod model;
mod resources;
mod view;

use reuben_api::authoring::{
    set_instrument_constant, set_instrument_description, set_instrument_input, set_instrument_name,
    set_instrument_node_description, Answer, EditResult, Refusal, Resources, SetInstrumentConstant,
    SetInstrumentDescription, SetInstrumentInput, SetInstrumentName, SetInstrumentNodeDescription,
};
use reuben_api::engine::{send_live_controls, Channel, ControlSendMessage, SendLiveControls};
use serde_json::Value;

use self::interface::{Adding, PipeEdit};
use self::model::{fields, pipe_field, Control, Drafts, Holds, Step, Target};
use self::resources::ResourceEdit;
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
    Pipe(PipeEdit),
    Resource(ResourceEdit),
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
            Edit::Pipe(edit) => edit.run(source, store, expect),
            Edit::Resource(edit) => edit.run(source, store, expect),
        }
    }
}

/// What the inspector keeps between frames.
#[derive(Debug, Default)]
pub struct Inspector {
    holds: Holds,
    drafts: Drafts,
    adding: Adding,
    resource: resources::Draft,
    /// Why the last live send did not reach the engine, until an edit or a send does.
    error: Option<String>,
    /// How many times a verb has been run to write the document.
    #[cfg(test)]
    writes: usize,
}

impl Inspector {
    /// Forget what is held and typed: the document they were set against cannot be read, so
    /// there is nothing to write them to.
    fn unreadable(&mut self) {
        self.holds.cancel();
        self.drafts.cancel();
    }

    /// Do what a number field asked for this frame.
    fn step(&mut self, step: Step, session: &mut Session, channel: &dyn Fn() -> Option<Channel>) {
        match step {
            Step::Live(target, value) => {
                if !session.trail().is_empty() {
                    return;
                }
                if let Some(message) = target.live(value) {
                    self.live(message, channel);
                }
            }
            Step::Commit {
                target,
                value,
                expect,
            } => {
                let held = expect.is_some();
                let literal = target.literal(value);
                let edit = Edit::Value {
                    target: target.clone(),
                    value: literal,
                };
                if !self.commit(edit, expect, session, channel) && held {
                    self.restore(&target, session, channel);
                }
            }
        }
    }

    /// Set `target` live back to what the file holds for it, after a held value that was sent live
    /// failed to be written: the engine would otherwise go on playing a value the file does not
    /// hold. Sends nothing when the file no longer has the input or feeds it from a wire.
    fn restore(
        &mut self,
        target: &Target,
        session: &Session,
        channel: &dyn Fn() -> Option<Channel>,
    ) {
        if !session.trail().is_empty() {
            return;
        }
        let Ok(top) = &session.viewed().read else {
            return;
        };
        let node = (top.nodes.iter())
            .find(|node| node.address == target.address)
            .and_then(|node| {
                fields(node)
                    .into_iter()
                    .find(|field| field.target == *target)
            });
        let pipe = || {
            (top.inputs.iter())
                .map(|pipe| pipe_field(&pipe.port))
                .find(|field| field.target == *target)
        };
        let held = node.or_else(pipe).and_then(|field| match field.control {
            Control::Number { value, .. } => target.live(value),
            _ => None,
        });
        if let Some(message) = held {
            self.live(message, channel);
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

    /// Write `edit` to the viewed document, against the content hash `expect`, or against the
    /// document on screen now when `None`. Returns whether it landed; why it did not is the
    /// session's [`Feedback`](crate::feedback::Feedback).
    fn commit(
        &mut self,
        edit: Edit,
        expect: Option<String>,
        session: &mut Session,
        channel: &dyn Fn() -> Option<Channel>,
    ) -> bool {
        #[cfg(test)]
        {
            self.writes += 1;
        }
        let source = session.viewed().source.clone();
        let added = edit.clone();
        let verb = |store: &dyn Resources, expect| edit.run(&source, store, expect);
        let result = match expect {
            Some(expect) => edit::write_against(session, channel, &source, expect, verb),
            None => edit::write(session, channel, &source, verb),
        };
        self.error = None;
        let landed = matches!(result, Ok(result) if result.written);
        match (landed, added) {
            (true, Edit::Pipe(added)) => self.adding.added(&added),
            (true, Edit::Resource(ResourceEdit::Add { .. })) => self.resource.added(),
            _ => {}
        }
        landed
    }
}

#[cfg(test)]
mod tests {
    use std::net::UdpSocket;
    use std::path::PathBuf;
    use std::time::Duration;

    use reuben_api::authoring::{
        read_instrument, rename_instrument_node, InputValue, PortInfo, ReadInstrument,
        RenameInstrumentNode, StructureNode, WireSource,
    };
    use reuben_api::render::Arg;
    use reuben_api::FsResolver;
    use reuben_native::engine::{EngineConfig, Instrument};
    use reuben_native::osc;
    use reuben_native::test_support::{start_headless, within};
    use serde_json::json;

    use super::interface::Meta;
    use super::*;
    use crate::canvas::Gesture;
    use crate::controls::Controls;
    use crate::engine::Engine;
    use crate::feedback::Feedback;
    use crate::session::test_support::{Rig, Scratch};
    use crate::session::Status;

    const HANG_SECS: u64 = 60;

    /// The instrument the tests open: its `/map_cutoff` sets the filter's highest cutoff with a
    /// literal, and its `/voicer` sets a constant.
    const INSTRUMENT: &str = "strum-harp.json";

    fn node(rig: &Rig, address: &str) -> StructureNode {
        let top = (rig.session.viewed().read.as_ref()).expect("the viewed document reads");
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
        let held = (node.inputs.iter())
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

    /// One frame of `target`'s number field as the view draws it: over the session's current read
    /// of the document, showing `value`, held or not.
    fn frame(rig: &mut Rig, inspector: &mut Inspector, target: &Target, value: f64, holding: bool) {
        let top = (rig.session.viewed().read.clone()).expect("the viewed document reads");
        let document = match fields(&node(rig, &target.address))
            .into_iter()
            .find(|f| f.target.name == target.name)
            .map(|f| f.control)
        {
            Some(Control::Number { value, .. }) => value,
            other => panic!("{target:?} is {other:?}"),
        };
        inspector.holds.begin();
        let mut steps =
            (inspector.holds).number(target, (document, &top.hash), value, holding, holding);
        steps.extend(inspector.holds.end());
        let channel = rig.channel_source();
        for step in steps {
            inspector.step(step, &mut rig.session, &channel);
        }
    }

    /// Hold `target` and move it through `values`, without letting go.
    fn hold(rig: &mut Rig, inspector: &mut Inspector, target: &Target, values: &[f64]) {
        for &value in values {
            frame(rig, inspector, target, value, true);
            assert_eq!(inspector.error, None);
        }
    }

    fn release(rig: &mut Rig, inspector: &mut Inspector, target: &Target, value: f64) {
        frame(rig, inspector, target, value, false);
    }

    /// The write `reuben-mcp` makes while the inspector holds a value.
    fn sidecar(rig: &mut Rig, write: impl FnOnce(&str, &FsResolver)) {
        let source = rig.session.tree().top().source.clone();
        write(&source, &FsResolver::for_document(&source).stat_only());
        let channel = rig.channel_source();
        rig.session.refresh(&channel);
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
            for &value in &values {
                frame(&mut rig, &mut inspector, &cutoff, value, true);
                assert_eq!(inspector.error, None);
                assert_eq!(inspector.writes, 0, "a held value is never written");
                assert_eq!(rig.swaps(), 0);
                assert_eq!(rig.session.tree().top().hash(), before.as_deref());
            }
            let received = rig.live_received(values.len());
            let sent: Vec<(String, Vec<Arg>)> = (received.into_iter())
                .map(|m| (m.address, m.args))
                .collect();
            let expected: Vec<(String, Vec<Arg>)> = (values.iter())
                .map(|&v| ("/map_cutoff/out_max".to_string(), vec![Arg::F32(v as f32)]))
                .collect();
            assert_eq!(
                sent, expected,
                "every move reached the render side, in order"
            );

            release(&mut rig, &mut inspector, &cutoff, 10350.0);
            assert_eq!(inspector.error, None);
            assert_eq!(inspector.writes, 1, "letting go writes once");
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
            assert_eq!(inspector.writes, 1);
        });
    }

    /// Inside a voice, a held number would name a node of the top-level instrument that happens to
    /// share its address: it is only written, and the write goes to the voice.
    #[test]
    fn a_number_held_inside_a_voice_is_never_sent_and_letting_go_writes_the_voice() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_voice", "acid-techno.json");
            rig.session
                .enter("/kick_v")
                .expect("/kick_v voices kick-voice");
            let voice = rig.lib.join("voices").join("kick-voice.json");
            assert_eq!(rig.session.viewed().path, voice);
            let top = rig.session.tree().top().hash().map(str::to_string);
            let before = rig.session.viewed().hash().map(str::to_string);
            let mut inspector = Inspector::default();
            let viewed = (rig.session.viewed().read.clone()).expect("the voice reads");
            let held = (viewed.nodes.iter())
                .flat_map(fields)
                .find(|f| !f.target.constant && matches!(f.control, Control::Number { .. }))
                .expect("the voice has a number to hold")
                .target;

            hold(&mut rig, &mut inspector, &held, &[60.0, 70.0]);
            release(&mut rig, &mut inspector, &held, 72.0);
            assert_eq!(inspector.error, None);
            assert_eq!(inspector.writes, 1);
            assert_eq!(rig.live_received(1), [], "nothing was sent live");
            assert_ne!(rig.session.viewed().hash(), before.as_deref());
            assert_eq!(rig.session.viewed().path, voice);
            assert_eq!(rig.session.tree().top().hash(), top.as_deref());
            rig.poll_until("the Swap of the write", |rig| rig.swaps() == 1);
            rig.settle();
            assert_eq!(rig.status(), &Status::Current);
        });
    }

    #[test]
    fn a_held_constant_is_written_once_on_release_and_never_sent() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_constant", INSTRUMENT);
            let mut inspector = Inspector::default();
            let voices = target(&rig, "/voicer", "voices");
            assert!(voices.constant && voices.integer, "{voices:?}");

            hold(&mut rig, &mut inspector, &voices, &[7.0, 5.6, 4.2]);
            assert_eq!(inspector.writes, 0);
            release(&mut rig, &mut inspector, &voices, 4.2);
            assert_eq!(inspector.error, None);
            assert_eq!(inspector.writes, 1);
            assert_eq!(literal(&rig, "/voicer", "voices").as_f64(), Some(4.0));
            rig.poll_until("the Swap of the write", |rig| rig.swaps() == 1);
            rig.settle();
            assert_eq!(rig.swaps(), 1);
            assert_eq!(rig.live_received(1), [], "a constant is never sent live");
        });
    }

    #[test]
    fn a_value_written_by_another_hand_during_a_hold_is_kept_and_the_release_reported() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_held_conflict", INSTRUMENT);
            let mut inspector = Inspector::default();
            let cutoff = target(&rig, "/map_cutoff", "out_max");

            hold(&mut rig, &mut inspector, &cutoff, &[900.0, 1200.0]);
            sidecar(&mut rig, |source, store| {
                let edit = set_instrument_input(
                    &SetInstrumentInput {
                        source: source.to_string(),
                        address: "/map_cutoff".into(),
                        input: "out_max".into(),
                        value: json!(5000.0),
                        expect: None,
                    },
                    store,
                )
                .expect("the verb ran")
                .output;
                assert!(edit.written, "{:?}", edit.report);
            });
            hold(&mut rig, &mut inspector, &cutoff, &[1500.0]);
            release(&mut rig, &mut inspector, &cutoff, 1500.0);

            assert_eq!(inspector.writes, 1);
            assert!(
                matches!(rig.session.feedback(), Some(Feedback::Conflict { .. })),
                "the conflict is reported: {:?}",
                rig.session.feedback()
            );
            assert_eq!(literal(&rig, "/map_cutoff", "out_max"), json!(5000.0));
            // The held moves, then the value the file holds, so the sound is the file again.
            let received = rig.live_received(4);
            let last = received.last().expect("live values were sent");
            assert_eq!(received.len(), 4, "{received:?}");
            assert_eq!(last.address, "/map_cutoff/out_max");
            assert_eq!(last.args, [Arg::F32(5000.0)]);
        });
    }

    #[test]
    fn a_release_after_another_hand_reshuffles_the_addresses_lands_on_no_node() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_renamed", INSTRUMENT);
            let mut inspector = Inspector::default();
            let cutoff = target(&rig, "/map_cutoff", "out_max");
            let resonance_max = literal(&rig, "/map_res", "out_max");

            hold(&mut rig, &mut inspector, &cutoff, &[9000.0]);
            sidecar(&mut rig, |source, store| {
                for (from, to) in [("/map_cutoff", "/map_old"), ("/map_res", "/map_cutoff")] {
                    let edit = rename_instrument_node(
                        &RenameInstrumentNode {
                            source: source.to_string(),
                            from: from.into(),
                            to: to.into(),
                            expect: None,
                        },
                        store,
                    )
                    .expect("the verb ran")
                    .output;
                    assert!(edit.written, "{:?}", edit.report);
                }
            });
            release(&mut rig, &mut inspector, &cutoff, 9000.0);

            assert!(
                matches!(rig.session.feedback(), Some(Feedback::Conflict { .. })),
                "the conflict is reported: {:?}",
                rig.session.feedback()
            );
            assert_eq!(literal(&rig, "/map_cutoff", "out_max"), resonance_max);
            assert_eq!(literal(&rig, "/map_old", "out_max"), json!(11000.0));
        });
    }

    #[test]
    fn a_hold_over_a_document_that_stops_reading_is_dropped_not_written_later() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_unreadable", INSTRUMENT);
            let mut inspector = Inspector::default();
            let cutoff = target(&rig, "/map_cutoff", "out_max");
            let path = rig.session.tree().top().path.clone();
            let text = std::fs::read_to_string(&path).expect("the instrument");
            let channel = rig.channel_source();

            hold(&mut rig, &mut inspector, &cutoff, &[9000.0]);
            std::fs::write(&path, "{ not json").expect("break the file");
            rig.session.refresh(&channel);
            assert!(rig.session.tree().top().read.is_err());
            inspector.unreadable();

            std::fs::write(&path, &text).expect("restore the file");
            rig.session.refresh(&channel);
            release(&mut rig, &mut inspector, &cutoff, 9000.0);
            assert_eq!(inspector.writes, 0);
            assert_eq!(literal(&rig, "/map_cutoff", "out_max"), json!(11000.0));
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
            let other = (variants.iter())
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
                inspector.commit(edit.clone(), None, &mut rig.session, &channel);
                assert_eq!(inspector.error, None, "{edit:?}");
            }
            assert_eq!(inspector.writes, 4);

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
                Some(top.hash.clone()),
                &mut rig.session,
                &channel,
            );
            assert_eq!(inspector.error, None);
            assert_eq!(node(&rig, "/map_cutoff").description, None);

            inspector.commit(
                Edit::Description("against a stale read".into()),
                Some(top.hash),
                &mut rig.session,
                &channel,
            );
            assert!(
                matches!(rig.session.feedback(), Some(Feedback::Conflict { .. })),
                "a stale draft is a conflict: {:?}",
                rig.session.feedback()
            );
            assert_eq!(
                (rig.session
                    .tree()
                    .top()
                    .read
                    .as_ref()
                    .expect("reads")
                    .description)
                    .as_deref(),
                Some("edited in the inspector")
            );
        });
    }

    #[test]
    fn a_refused_value_is_reported_and_nothing_is_written() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_refused", INSTRUMENT);
            let mut inspector = Inspector::default();
            let channel = rig.channel_source();
            let before = rig.session.tree().clone();
            let curve = target(&rig, "/map_cutoff", "curve");
            inspector.commit(
                Edit::Value {
                    target: curve,
                    value: json!("NoSuchCurve"),
                },
                None,
                &mut rig.session,
                &channel,
            );
            let rejected = rig.session.feedback().expect("the rejection is reported");
            let top = before.top().source.clone();
            let at_fault = rejected.rejected(&top);
            assert!(
                (at_fault.iter()).any(|d| d.node.as_deref() == Some("/map_cutoff")),
                "{rejected:?}"
            );
            assert_eq!(rig.session.tree(), &before);
            rig.settle();
            assert_eq!(rig.swaps(), 0);
        });
    }

    /// A subpatch whose child echoes its `level` pipe on OSC-out, so a live value that reached the
    /// pipe comes back out.
    const CHILD: &str = r#"{"format_version":3,"instrument":"child",
        "interface":{"inputs":{"level":{"type":"f32","default":0.2,"min":0.0,"max":1.0}},
                     "outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[{"type":"oscillator","address":"/osc"},
                 {"type":"osc_out","address":"/level_echo","inputs":{"in":{"from":"/level"}}}]}"#;
    const PARENT: &str = r#"{"format_version":3,"instrument":"parent",
        "resources":{"child":"child.json"},
        "interface":{"outputs":{"out":{"from":"/sub.out"}}},
        "nodes":[{"type":"subpatch","address":"/sub","patch":"child","inputs":{"level":0.2}}]}"#;

    #[test]
    fn a_live_value_on_a_subpatch_port_reaches_the_childs_pipe() {
        let scratch = Scratch::new("inspector_subpatch");
        let parent: PathBuf = scratch.path("parent.json");
        std::fs::write(scratch.path("child.json"), CHILD).expect("the child");
        std::fs::write(&parent, PARENT).expect("the parent");

        within(HANG_SECS, move || {
            let source = parent.display().to_string();
            let top = read_instrument(
                &ReadInstrument {
                    source: source.clone(),
                },
                &FsResolver::for_document(&source).stat_only(),
            )
            .expect("the parent reads")
            .output;
            assert!(top.report.ok, "{:?}", top.report);
            let sub = (top.nodes.iter())
                .find(|n| n.address == "/sub")
                .expect("/sub");
            let level = (fields(sub).into_iter())
                .find(|f| f.target.name == "level")
                .expect("the child's level pipe")
                .target;

            let sink = UdpSocket::bind("127.0.0.1:0").expect("an OSC-out sink");
            sink.set_read_timeout(Some(Duration::from_secs(5)))
                .expect("a read timeout");
            let mut engine = Engine::start_with(
                EngineConfig {
                    osc_in: None,
                    osc_out: Some(sink.local_addr().expect("bound").to_string()),
                    structure: None,
                    ..EngineConfig::new(Instrument::Path(parent))
                },
                start_headless,
            );
            assert!(engine.channel().is_some(), "{:?}", engine.report());
            let channel = || engine.channel();

            let mut inspector = Inspector::default();
            // Addressed as an operator's input would be, the value reaches nothing.
            let mut stray = level.live(0.25).expect("a runtime input");
            stray.address = format!("{}/{}", level.address, level.name);
            inspector.live(stray, &channel);
            inspector.live(level.live(0.75).expect("a runtime input"), &channel);
            assert_eq!(inspector.error, None);

            let mut buf = [0u8; 1024];
            loop {
                let n = sink.recv(&mut buf).expect("the echo of the live value");
                let args: Vec<Arg> = (osc::decode(&buf[..n]).expect("decode").into_iter())
                    .flat_map(|m| m.args)
                    .collect();
                assert!(!args.contains(&Arg::F32(0.25)), "the stray send was routed");
                if args.contains(&Arg::F32(0.75)) {
                    break;
                }
            }
            engine.shutdown();
        });
    }

    fn pipe(rig: &Rig, name: &str) -> PortInfo {
        let top = (rig.session.tree().top().read.as_ref()).expect("the top reads");
        (top.inputs.iter())
            .find(|p| p.port.name == name)
            .unwrap_or_else(|| panic!("{name} is an interface input"))
            .port
            .clone()
    }

    /// Commit `edit` and wait until the engine plays what it wrote.
    fn landed(rig: &mut Rig, inspector: &mut Inspector, edit: Edit) {
        let channel = rig.channel_source();
        let wrote = inspector.commit(edit.clone(), None, &mut rig.session, &channel);
        assert!(wrote, "{edit:?}: {:?}", rig.session.feedback());
        played(rig);
    }

    fn played(rig: &mut Rig) {
        let hash = (rig.session.tree().top().hash())
            .expect("the top reads")
            .to_string();
        rig.poll_until("the engine playing the edit", |rig| rig.playing() == hash);
    }

    fn gesture(rig: &mut Rig, gesture: Gesture) {
        let channel = rig.channel_source();
        let edit = gesture
            .apply(&mut rig.session, &channel)
            .expect("the verb ran");
        assert!(edit.written, "{gesture:?}: {:?}", edit.report);
        played(rig);
    }

    #[test]
    fn a_seed_is_made_audible_and_given_a_control_from_the_inspector_and_the_canvas() {
        within(HANG_SECS * 2, || {
            let mut rig = Rig::fresh("inspector_from_scratch", "scratch");
            let mut inspector = Inspector::default();
            let top = |rig: &Rig| rig.session.tree().top().read.clone().expect("reads");
            assert!(top(&rig).outputs.is_empty());

            gesture(
                &mut rig,
                Gesture::Add {
                    address: "/osc".into(),
                    type_name: "oscillator".into(),
                },
            );
            let output = Adding::default().output(&top(&rig)).expect("a feed");
            assert_eq!(
                output,
                PipeEdit::AddOutput {
                    name: "out".into(),
                    from: "/osc.audio".into()
                }
            );
            landed(&mut rig, &mut inspector, Edit::Pipe(output));
            let boundary = top(&rig).outputs;
            assert_eq!(boundary.len(), 1);
            assert_eq!(
                boundary[0].from,
                WireSource::NodeOutput {
                    node: "/osc".into(),
                    port: "audio".into()
                }
            );

            // Expose the oscillator's pitch as a control.
            landed(
                &mut rig,
                &mut inspector,
                Edit::Pipe(PipeEdit::AddInput {
                    name: "pitch".into(),
                    type_name: "f32".into(),
                    value: Some(json!(220.0)),
                }),
            );
            gesture(
                &mut rig,
                Gesture::Wire {
                    address: "/osc".into(),
                    input: "freq".into(),
                    from: "/pitch".into(),
                },
            );
            landed(
                &mut rig,
                &mut inspector,
                Edit::Pipe(PipeEdit::InputMeta {
                    name: "pitch".into(),
                    meta: Meta {
                        min: Some(20.0),
                        max: Some(2000.0),
                        curve: Some("exp".into()),
                        unit: Some("Hz".into()),
                        channel: None,
                    },
                }),
            );
            let port = pipe(&rig, "pitch");
            assert_eq!((port.min, port.max), (Some(20.0), Some(2000.0)));
            assert_eq!(port.curve.as_deref(), Some("exponential"));
            assert_eq!(port.unit, "Hz");
            assert_eq!(Controls::open(Some(&rig.top), None).binds(), ["pitch"]);

            // Its value is swept live on the pipe's own input, and written once on release.
            let value = pipe_field(&port).target;
            assert_eq!(value.address, "/pitch");
            let mut frame = |rig: &mut Rig, value: f64, holding: bool| {
                let top = top(rig);
                let held = pipe_field(&pipe(rig, "pitch")).target;
                inspector.holds.begin();
                let mut steps =
                    (inspector.holds).number(&held, (220.0, &top.hash), value, holding, holding);
                steps.extend(inspector.holds.end());
                let channel = rig.channel_source();
                for step in steps {
                    inspector.step(step, &mut rig.session, &channel);
                }
            };
            frame(&mut rig, 330.0, true);
            frame(&mut rig, 440.0, true);
            let received = rig.live_received(2);
            assert_eq!(received.len(), 2, "{received:?}");
            assert!(received.iter().all(|m| m.address == "/pitch/in"));
            frame(&mut rig, 440.0, false);
            played(&mut rig);
            assert_eq!(rig.session.feedback(), None);
            assert_eq!(pipe(&rig, "pitch").default, Some(json!(440.0)));

            // A parent nesting it sees the control on its subpatch node's face.
            let parent = rig.lib.join("parent.json");
            std::fs::write(
                &parent,
                r#"{"format_version":3,"instrument":"parent",
                    "resources":{"child":"scratch.json"},
                    "nodes":[{"type":"subpatch","address":"/sub","patch":"child"}]}"#,
            )
            .expect("the parent");
            let source = parent.display().to_string();
            let read = read_instrument(
                &ReadInstrument {
                    source: source.clone(),
                },
                &FsResolver::for_document(&source).stat_only(),
            )
            .expect("the parent reads")
            .output;
            let face: Vec<&str> = (read.nodes[0].ports.inputs.iter())
                .map(|p| p.name.as_str())
                .collect();
            assert_eq!(face, ["pitch"]);

            // A second node takes over the output, and both pipes come out again.
            gesture(
                &mut rig,
                Gesture::Add {
                    address: "/filter".into(),
                    type_name: "filter".into(),
                },
            );
            gesture(
                &mut rig,
                Gesture::Feed {
                    name: "out".into(),
                    from: "/filter.audio".into(),
                },
            );
            assert_eq!(
                top(&rig).outputs[0].from,
                WireSource::NodeOutput {
                    node: "/filter".into(),
                    port: "audio".into()
                }
            );
            landed(
                &mut rig,
                &mut inspector,
                Edit::Pipe(PipeEdit::RemoveInput("pitch".into())),
            );
            landed(
                &mut rig,
                &mut inspector,
                Edit::Pipe(PipeEdit::RemoveOutput("out".into())),
            );
            let after = top(&rig);
            assert!(after.inputs.is_empty() && after.outputs.is_empty());
            assert_eq!(
                Controls::open(Some(&rig.top), None).binds(),
                [] as [&str; 0]
            );
        });
    }

    #[test]
    fn an_input_added_to_a_viewed_subpatch_is_a_port_on_the_parents_subpatch_node() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_subpatch_pipe", "mic-space.json");
            rig.session
                .enter("/space")
                .expect("/space nests space.json");
            let child = rig.lib.join("patches").join("space.json");
            assert_eq!(rig.session.viewed().path, child);
            let face = |rig: &Rig| -> Vec<String> {
                let top = (rig.session.tree().top().read.as_ref()).expect("the top reads");
                let space = (top.nodes.iter())
                    .find(|n| n.address == "/space")
                    .expect("the subpatch node");
                (space.ports.inputs.iter())
                    .map(|p| p.name.clone())
                    .collect()
            };
            assert!(!face(&rig).iter().any(|p| p == "width"));
            let mut inspector = Inspector::default();
            let channel = rig.channel_source();

            let wrote = inspector.commit(
                Edit::Pipe(PipeEdit::AddInput {
                    name: "width".into(),
                    type_name: "f32".into(),
                    value: Some(json!(0.5)),
                }),
                None,
                &mut rig.session,
                &channel,
            );
            assert!(wrote, "{:?}", rig.session.feedback());
            assert_eq!(rig.session.viewed().path, child);
            let viewed = (rig.session.viewed().read.clone()).expect("the child reads");
            assert!(viewed.inputs.iter().any(|p| p.port.name == "width"));
            assert!(face(&rig).iter().any(|p| p == "width"), "{:?}", face(&rig));

            let wrote = inspector.commit(
                Edit::Pipe(PipeEdit::RemoveInput("width".into())),
                None,
                &mut rig.session,
                &channel,
            );
            assert!(wrote, "{:?}", rig.session.feedback());
            assert!(!face(&rig).iter().any(|p| p == "width"));
            rig.poll_until("the Swap of the writes", |rig| rig.swaps() >= 1);
            rig.settle();
            assert_eq!(rig.status(), &Status::Current);
        });
    }

    #[test]
    fn a_rejected_feed_is_the_sessions_feedback_not_the_inspectors_error() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_pipe_rejected", "default.json");
            let mut inspector = Inspector::default();
            let channel = rig.channel_source();
            let before = rig.session.tree().clone();
            let wrote = inspector.commit(
                Edit::Pipe(PipeEdit::Feed {
                    name: "out".into(),
                    from: "/ghost.audio".into(),
                }),
                None,
                &mut rig.session,
                &channel,
            );
            assert!(!wrote);
            assert_eq!(
                inspector.error, None,
                "a rejection is not a live-send failure"
            );
            assert_eq!(rig.session.tree(), &before);
            let feedback = rig
                .session
                .feedback()
                .expect("the rejection is held")
                .clone();
            let source = rig.session.viewed().source.clone();
            assert!(
                matches!(feedback, Feedback::Rejected { .. }),
                "{feedback:?}"
            );
            // The fault is the feed's: it is pinned on the output pipe the feed is written at,
            // since the node it names is one the document does not have.
            let at: Vec<_> = (feedback.rejected(&source).iter())
                .map(|diag| (diag.node.as_deref(), diag.port.as_deref()))
                .collect();
            assert_eq!(at, [(None, Some("out"))], "{feedback:?}");
        });
    }

    #[test]
    fn an_input_pipe_named_past_the_address_rule_is_rejected_naming_the_pipe() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_pipe_bad_name", "default.json");
            let mut inspector = Inspector::default();
            let channel = rig.channel_source();
            let before = rig.session.tree().clone();
            let wrote = inspector.commit(
                Edit::Pipe(PipeEdit::AddInput {
                    name: "my tone".into(),
                    type_name: "f32".into(),
                    value: Some(json!(0.5)),
                }),
                None,
                &mut rig.session,
                &channel,
            );
            assert!(!wrote);
            assert_eq!(rig.session.tree(), &before);
            let feedback = rig.session.feedback().expect("the rejection is held");
            let source = rig.session.viewed().source.clone();
            let errors = feedback.rejected(&source);
            assert!(
                errors
                    .iter()
                    .any(|d| d.port.as_deref() == Some("my tone")
                        && d.message.contains("whitespace")),
                "{feedback:?}"
            );
        });
    }

    #[test]
    fn an_add_row_that_names_no_pipe_is_refused_through_the_sessions_feedback() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("inspector_pipe_unnamed", "default.json");
            let mut inspector = Inspector::default();
            let channel = rig.channel_source();
            let why = Adding::default().input().expect_err("no name");
            view::refuse(&mut rig.session, why.clone());
            assert_eq!(
                rig.session.feedback(),
                Some(&Feedback::Refused {
                    source: rig.session.viewed().source.clone(),
                    message: why
                })
            );
            let landed = inspector.commit(
                Edit::Pipe(PipeEdit::RemoveOutput("out".into())),
                None,
                &mut rig.session,
                &channel,
            );
            assert!(landed);
            assert_eq!(rig.session.feedback(), None, "a landed edit lets it go");
        });
    }
}
