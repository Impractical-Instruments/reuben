//! What a finished gesture on the canvas asks of the document, and the one authoring verb each is.

use reuben_api::authoring::{
    add_instrument_node, remove_instrument_interface_input, remove_instrument_interface_output,
    remove_instrument_node, rename_instrument_node, set_instrument_layout, unwire_instrument_input,
    wire_instrument_input, wire_instrument_interface_output, AddInstrumentNode, EditResult,
    LayoutPosition, LayoutTarget, OutputTarget, Refusal, RemoveInstrumentInterfacePipe,
    RemoveInstrumentNode, RenameInstrumentNode, SetInstrumentLayout, UnwireInstrumentInput,
    WireInstrumentInput, WireInstrumentInterfaceOutput, WireSource, WireTarget,
};
use reuben_api::engine::Channel;

use super::graph::{Graph, Item, PortRef};
use crate::edit;
use crate::session::Session;

/// One edit of the document the session views.
#[derive(Debug, Clone, PartialEq)]
pub enum Gesture {
    /// Boxes dropped where they were dragged to.
    Move(Vec<LayoutPosition>),
    Add {
        address: String,
        type_name: String,
    },
    Remove {
        address: String,
    },
    Rename {
        from: String,
        to: String,
    },
    /// Input `input` of the node at `address`, wired from the wire-ref `from`.
    Wire {
        address: String,
        input: String,
        from: String,
    },
    Unwire {
        address: String,
        input: String,
    },
    /// The output pipe `name`, fed from the wire-ref `from` in place of what fed it.
    Feed {
        name: String,
        from: String,
    },
    RemoveInput {
        name: String,
    },
    RemoveOutput {
        name: String,
    },
}

impl Gesture {
    /// Make this edit through [`edit::write`]: one verb call, guarded by the hash the session holds
    /// for the document.
    pub fn apply(
        &self,
        session: &mut Session,
        channel: &dyn Fn() -> Option<Channel>,
    ) -> Result<EditResult, Refusal> {
        let source = session.viewed().source.clone();
        edit::write(session, channel, &source, |store, expect| {
            let source = source.clone();
            match self.clone() {
                Gesture::Move(positions) => set_instrument_layout(
                    &SetInstrumentLayout {
                        source,
                        positions,
                        expect,
                    },
                    store,
                ),
                Gesture::Add { address, type_name } => add_instrument_node(
                    &AddInstrumentNode {
                        source,
                        address,
                        type_name,
                        expect,
                        ..Default::default()
                    },
                    store,
                ),
                Gesture::Remove { address } => remove_instrument_node(
                    &RemoveInstrumentNode {
                        source,
                        address,
                        expect,
                    },
                    store,
                ),
                Gesture::Rename { from, to } => rename_instrument_node(
                    &RenameInstrumentNode {
                        source,
                        from,
                        to,
                        expect,
                    },
                    store,
                ),
                Gesture::Wire {
                    address,
                    input,
                    from,
                } => wire_instrument_input(
                    &WireInstrumentInput {
                        source,
                        address,
                        input,
                        from,
                        expect,
                    },
                    store,
                ),
                Gesture::Unwire { address, input } => unwire_instrument_input(
                    &UnwireInstrumentInput {
                        source,
                        address,
                        input,
                        expect,
                    },
                    store,
                ),
                Gesture::Feed { name, from } => wire_instrument_interface_output(
                    &WireInstrumentInterfaceOutput {
                        source,
                        name,
                        from,
                        expect,
                    },
                    store,
                ),
                Gesture::RemoveInput { name } => remove_instrument_interface_input(
                    &RemoveInstrumentInterfacePipe {
                        source,
                        name,
                        expect,
                    },
                    store,
                ),
                Gesture::RemoveOutput { name } => remove_instrument_interface_output(
                    &RemoveInstrumentInterfacePipe {
                        source,
                        name,
                        expect,
                    },
                    store,
                ),
            }
        })
    }
}

/// What `set_instrument_layout` calls the box standing for `item`.
pub fn layout_target(item: &Item) -> LayoutTarget {
    match item {
        Item::Node(address) => LayoutTarget::Node(address.clone()),
        // An input pipe mints the node address `/<name>`.
        Item::Input(name) => LayoutTarget::Node(format!("/{name}")),
        Item::Output(name) => LayoutTarget::Output(OutputTarget {
            output: name.clone(),
        }),
    }
}

/// The wire-ref `wire_instrument_input` takes for `from`.
pub fn wire_ref(from: &WireSource) -> String {
    match from {
        WireSource::NodeOutput { node, port } => format!("{node}.{port}"),
        WireSource::InterfaceInput { name } => format!("/{name}"),
    }
}

/// Where a wire from the output `from` leaves, as `wire_compatibility` takes it. `None` for a port
/// no wire leaves from.
pub fn wire_source(graph: &Graph, from: PortRef) -> Option<WireSource> {
    let b = graph.boxes.get(from.node)?;
    match &b.item {
        Item::Node(node) => Some(WireSource::NodeOutput {
            node: node.clone(),
            port: b.outputs.get(from.port)?.name.clone(),
        }),
        Item::Input(name) => Some(WireSource::InterfaceInput { name: name.clone() }),
        Item::Output(_) => None,
    }
}

/// The node address and input name of the input `to`. `None` for an output pipe's, which is fed
/// rather than wired and never unwired: a pipe has its one feed from the moment it is added.
pub fn input_end(graph: &Graph, to: PortRef) -> Option<(String, String)> {
    let b = graph.boxes.get(to.node)?;
    match &b.item {
        Item::Node(address) => Some((address.clone(), b.inputs.get(to.port)?.name.clone())),
        Item::Input(_) | Item::Output(_) => None,
    }
}

/// The edit a wire from `from` dropped on the input `to` makes: wiring a node's input, or feeding
/// an output pipe.
pub fn wire_to(graph: &Graph, from: &WireSource, to: PortRef) -> Option<Gesture> {
    let from = wire_ref(from);
    match &graph.boxes.get(to.node)?.item {
        Item::Output(name) => Some(Gesture::Feed {
            name: name.clone(),
            from,
        }),
        _ => {
            let (address, input) = input_end(graph, to)?;
            Some(Gesture::Wire {
                address,
                input,
                from,
            })
        }
    }
}

/// The box standing for the target of a wire compatibility verdict, and its input port.
pub fn target_ref(graph: &Graph, target: &WireTarget) -> Option<PortRef> {
    match target {
        WireTarget::NodeInput { node, port } => input_ref(graph, node, port),
        WireTarget::InterfaceOutput { name } => Some(PortRef {
            node: graph.index_of(&Item::Output(name.clone()))?,
            port: 0,
        }),
    }
}

/// The input `port` of the node at `node`, when the canvas draws it.
pub fn input_ref(graph: &Graph, node: &str, port: &str) -> Option<PortRef> {
    let index = graph.index_of(&Item::Node(node.to_string()))?;
    let port = graph.boxes[index]
        .inputs
        .iter()
        .position(|p| p.name == port)?;
    Some(PortRef { node: index, port })
}

/// An address for a new node of type `type_name` that no node or input pipe of `graph` has:
/// `/<type>`, else the first free `/<type>2`, `/<type>3`, ….
pub fn free_address(graph: &Graph, type_name: &str) -> String {
    let taken = |address: &str| {
        graph.boxes.iter().any(|b| match &b.item {
            Item::Node(a) => a == address,
            Item::Input(name) => address.strip_prefix('/') == Some(name.as_str()),
            Item::Output(_) => false,
        })
    };
    let base = format!("/{type_name}");
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}{n}"))
        .find(|address| !taken(address))
        .expect("an unbounded range has a free number")
}

/// What the user typed as a node's new address, as an address: trimmed, with the leading `/` an
/// address needs added when it was left off.
pub fn typed_address(text: &str) -> String {
    let text = text.trim();
    if text.starts_with('/') {
        text.to_string()
    } else {
        format!("/{text}")
    }
}

#[cfg(test)]
mod tests {
    use reuben_api::authoring::{
        set_instrument_name, validate_instrument, SetInstrumentName, ValidateInstrument,
    };
    use reuben_api::FsResolver;
    use reuben_native::test_support::within;

    use super::super::graph::tests::small;
    use super::*;
    use crate::edit::test_support::writes;
    use crate::edit::{restore, Restored};
    use crate::session::test_support::Rig;
    use crate::session::{Direction, Listing, Status};

    const HANG_SECS: u64 = 120;

    #[test]
    fn a_new_address_is_the_type_name_or_the_first_free_number_after_it() {
        let graph = Graph::from_structure(&small());
        assert_eq!(free_address(&graph, "lowpass"), "/lowpass");
        assert_eq!(free_address(&graph, "osc"), "/osc2");
        // An input pipe's name is an address too.
        assert_eq!(free_address(&graph, "cutoff"), "/cutoff2");
        // An output pipe's is not.
        assert_eq!(free_address(&graph, "main"), "/main");
    }

    #[test]
    fn a_typed_address_gets_its_slash() {
        assert_eq!(typed_address(" lead "), "/lead");
        assert_eq!(typed_address("/lead"), "/lead");
    }

    #[test]
    fn each_kind_of_box_is_its_own_layout_target_and_wire_source() {
        assert_eq!(
            layout_target(&Item::Node("/osc".into())),
            LayoutTarget::Node("/osc".into())
        );
        assert_eq!(
            layout_target(&Item::Input("cutoff".into())),
            LayoutTarget::Node("/cutoff".into())
        );
        assert_eq!(
            layout_target(&Item::Output("main".into())),
            LayoutTarget::Output(OutputTarget {
                output: "main".into()
            })
        );
        let node = WireSource::NodeOutput {
            node: "/osc".into(),
            port: "audio".into(),
        };
        assert_eq!(wire_ref(&node), "/osc.audio");
        let pipe = WireSource::InterfaceInput {
            name: "cutoff".into(),
        };
        assert_eq!(wire_ref(&pipe), "/cutoff");
    }

    /// Make `gesture` and wait until the engine plays what it wrote: one write, one Swap, and a
    /// file that validates.
    fn step(rig: &mut Rig, gesture: Gesture) {
        let before = rig.swaps();
        let written_before = writes();
        let channel = rig.channel_source();
        let result = gesture
            .apply(&mut rig.session, &channel)
            .unwrap_or_else(|refusal| panic!("{gesture:?}: {refusal}"));
        assert!(result.written, "{gesture:?}: {:?}", result.report);
        assert_eq!(writes() - written_before, 1, "{gesture:?} made one write");
        let source = rig.top.display().to_string();
        let valid = validate_instrument(
            &ValidateInstrument {
                source: source.clone(),
            },
            rig.session.history(&source),
        )
        .expect("the file reads")
        .output;
        assert!(valid.ok, "{gesture:?} left {valid:?}");
        let hash = result.hash.clone();
        rig.poll_until("the engine playing the edit", |rig| rig.playing() == hash);
        rig.settle();
        assert_eq!(rig.swaps(), before + 1, "{gesture:?}");
        assert_eq!(rig.status(), &Status::Current, "{gesture:?}");
    }

    #[test]
    fn a_small_synth_is_built_one_gesture_and_one_swap_at_a_time() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("gesture_build", "default.json");
            let add = |address: &str, type_name: &str| Gesture::Add {
                address: address.into(),
                type_name: type_name.into(),
            };
            let wire = |address: &str, input: &str, from: &str| Gesture::Wire {
                address: address.into(),
                input: input.into(),
                from: from.into(),
            };
            step(&mut rig, add("/osc", "oscillator"));
            step(&mut rig, wire("/out", "audio", "/osc.audio"));
            step(&mut rig, add("/filter", "filter"));
            step(&mut rig, wire("/filter", "audio", "/osc.audio"));
            step(&mut rig, wire("/out", "audio", "/filter.audio"));
            step(&mut rig, add("/lfo", "lfo"));
            step(&mut rig, wire("/filter", "cutoff", "/lfo.out"));
            step(
                &mut rig,
                Gesture::Rename {
                    from: "/filter".into(),
                    to: "/lowpass".into(),
                },
            );
            step(
                &mut rig,
                Gesture::Move(vec![LayoutPosition {
                    target: LayoutTarget::Node("/lowpass".into()),
                    x: 320.0,
                    y: 40.0,
                }]),
            );
            step(
                &mut rig,
                Gesture::Unwire {
                    address: "/lowpass".into(),
                    input: "cutoff".into(),
                },
            );
            step(
                &mut rig,
                Gesture::Remove {
                    address: "/lfo".into(),
                },
            );
            step(
                &mut rig,
                Gesture::Remove {
                    address: "/voicer".into(),
                },
            );

            let top = rig.session.tree().top().read.clone().expect("it reads");
            let mut nodes: Vec<&str> = top.nodes.iter().map(|n| n.address.as_str()).collect();
            nodes.sort_unstable();
            assert_eq!(nodes, ["/lowpass", "/osc", "/out"]);
        });
    }

    /// The acceptance case for editing a shared voice: from inside acid-techno, a gesture on
    /// `kick-voice` writes that file and no other, the top-level instrument is swapped in, and the
    /// three library instruments playing the voice are named.
    #[test]
    fn a_gesture_inside_a_voice_writes_the_voice_and_swaps_the_instrument() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("gesture_voice", "acid-techno.json");
            let voice = rig.lib.join("voices").join("kick-voice.json");
            let top = rig.session.tree().top().hash().map(str::to_string);
            let playing = rig.playing();
            rig.session
                .enter("/kick_v")
                .expect("/kick_v voices kick-voice");
            assert_eq!(rig.session.viewed().path, voice);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            while rig.session.users() == Some(&Listing::Checking) {
                assert!(std::time::Instant::now() < deadline, "never listed");
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            assert_eq!(
                rig.session.users(),
                Some(&Listing::Listed(Ok(vec![
                    "acid-techno".to_string(),
                    "euclidean-drums".to_string(),
                    "groovebox".to_string()
                ])))
            );

            let channel = rig.channel_source();
            let result = Gesture::Remove {
                address: "/sweep_cv".into(),
            }
            .apply(&mut rig.session, &channel)
            .expect("the verb ran");
            assert!(result.written, "{:?}", result.report);
            assert_eq!(
                rig.session.viewed().path,
                voice,
                "the view stays in the voice"
            );
            assert_eq!(rig.session.viewed().hash(), Some(result.hash.as_str()));
            assert_eq!(rig.session.tree().top().hash(), top.as_deref());
            rig.poll_until("the Swap of the write", |rig| rig.swaps() == 1);
            rig.settle();
            assert_eq!(rig.swaps(), 1);
            assert_eq!(rig.status(), &Status::Current);
            assert_eq!(
                rig.playing(),
                playing,
                "the top-level document is the one playing; only its voice moved"
            );

            rig.session.leave(0);
            assert_eq!(rig.session.viewed().hash(), top.as_deref());
        });
    }

    #[test]
    fn a_refused_gesture_writes_nothing_and_swaps_nothing() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("gesture_refused", "default.json");
            let before = rig.session.tree().clone();
            let channel = rig.channel_source();
            // A note pipe into an audio input: the wire's types do not match.
            let result = Gesture::Wire {
                address: "/out".into(),
                input: "audio".into(),
                from: "/notes".into(),
            }
            .apply(&mut rig.session, &channel)
            .expect("the verb ran");
            assert!(!result.written);
            assert!(!result.report.errors.is_empty());
            rig.settle();
            assert_eq!(rig.swaps(), 0);
            assert_eq!(rig.session.tree(), &before);
        });
    }

    #[test]
    fn a_gesture_made_against_a_view_the_file_has_moved_on_from_is_a_conflict() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("gesture_stale", "default.json");
            let source = rig.top.display().to_string();
            // Another writer's edit, which the session has not read yet.
            let theirs = set_instrument_name(
                &SetInstrumentName {
                    source: source.clone(),
                    name: "renamed-elsewhere".to_string(),
                    expect: None,
                },
                &FsResolver::for_document(&source).stat_only(),
            )
            .expect("the verb ran")
            .output;
            assert!(theirs.written);

            let written_before = writes();
            let channel = rig.channel_source();
            let ours = Gesture::Add {
                address: "/osc".into(),
                type_name: "oscillator".into(),
            }
            .apply(&mut rig.session, &channel)
            .expect("the verb ran");
            assert!(!ours.written, "{:?}", ours.report);
            assert_eq!(ours.hash, theirs.hash, "nothing of ours landed");
            assert_eq!(writes(), written_before);
            let on_disk = rig.session.tree().top().read.clone().expect("it reads");
            assert_eq!(on_disk.hash, theirs.hash);
            assert!(on_disk.nodes.iter().all(|n| n.address != "/osc"));
        });
    }

    #[test]
    fn an_add_and_a_move_are_each_one_step_to_undo_and_redo() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("gesture_undo", "default.json");
            let channel = rig.channel_source();
            let top = |rig: &Rig| rig.session.tree().top().read.clone().expect("it reads");
            let mut hashes = vec![top(&rig).hash];
            for gesture in [
                Gesture::Add {
                    address: "/osc".into(),
                    type_name: "oscillator".into(),
                },
                Gesture::Move(vec![LayoutPosition {
                    target: LayoutTarget::Node("/osc".into()),
                    x: 320.0,
                    y: 40.0,
                }]),
            ] {
                let result = gesture
                    .apply(&mut rig.session, &channel)
                    .expect("the verb ran");
                assert!(result.written, "{gesture:?}: {:?}", result.report);
                hashes.push(result.hash);
            }

            for hash in hashes[..2].iter().rev() {
                let undone = restore(&mut rig.session, &channel, Direction::Undo);
                assert!(matches!(undone, Restored::Landed), "{undone:?}");
                assert_eq!(&top(&rig).hash, hash);
            }
            assert!(top(&rig).nodes.iter().all(|n| n.address != "/osc"));

            for hash in &hashes[1..] {
                let redone = restore(&mut rig.session, &channel, Direction::Redo);
                assert!(matches!(redone, Restored::Landed), "{redone:?}");
                assert_eq!(&top(&rig).hash, hash);
            }
            rig.settle();
            assert_eq!(rig.status(), &Status::Current);
            assert_eq!(rig.playing(), hashes[2], "the engine plays the redone file");
        });
    }
}
