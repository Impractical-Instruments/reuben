//! The render surface: what a host drives per block, however it likes — a tight loop for offline
//! render, a device callback, a Web Audio render callback.
//!
//! Nothing here converts. A handle that crosses this boundary is re-exported or passed through
//! inline, because anything else is paid once per block and shows up as an instruction-count
//! regression rather than as a design opinion.
//!
//! The shape a host wires up is the same everywhere: something builds a [`RenderSide`], the
//! callback wraps it in a [`RenderSlot`] and calls [`fill`](RenderSlot::fill) or
//! [`fill_duplex`](RenderSlot::fill_duplex). The install mailbox, the survivor transplant and the
//! master-gain ramp all happen inside those calls, so a host never sequences them.
//!
//! What differs between hosts is graph construction, not rendering. [`install_graph`] takes a
//! [`Graph`] built in Rust: no document, no serde, no filesystem. The document-loading route is
//! `engine::install_initial`, which needs `authoring` and `render` both. Either way a host drives
//! the same [`RenderSlot`].
//!
//! [`RenderSide::engine`] is `pub`, so `Engine`'s whole API comes through with it. Drive
//! [`RenderSlot`] instead: it owns the install mailbox and the master-gain ramp, which a bare
//! `Engine` has neither of.
//!
//! `Registry` stays out for size: `Registry::builtin()` flattens a `const` census naming every
//! operator's `make`/`descriptor` fn pointer, so exposing it pulls the whole operator set into an
//! image where size binds. Re-exporting [`mod@operators`] costs nothing — only the operators a
//! graph actually constructs are codegen'd.

/// The render side of a fresh pair — the initial Engine plus the mailbox a swap installs through.
/// [`RenderSlot::new`] takes it; nothing else does.
pub use reuben_core::coordinator::RenderSide;
/// The RT-side handle. Every per-block call a host makes is one of its methods.
pub use reuben_core::coordinator::RenderSlot;
/// The single-slot atomic swap channel underneath the install mailbox, re-exported as the
/// **primitive** it is: a host with its own RT-side payload (the native door's device output map)
/// builds a parallel channel out of it rather than inventing a second lock-free mailbox.
pub use reuben_core::coordinator::{swap_pair, CoordinatorMailbox, RenderMailbox, SwapInFlight};
/// The graph a host builds in Rust, and the handle each `add` hands back. [`install_graph`]
/// consumes one; [`mod@operators`] is what it is built out of.
pub use reuben_core::graph::{Graph, NodeKey};
/// One control atom and the internal message carrying it. A host mints [`Arg`]s at its own foreign
/// edge (an OSC datagram, a MIDI event) and hands them to [`RenderSlot::queue_osc`]; a [`Message`]
/// is what comes back out of [`RenderSlot::drain_outbound`].
pub use reuben_core::message::{Arg, Message};
/// The port bounds [`Graph::connect`] and [`Graph::tap_output`] take, for a host writing its own
/// wiring helper over them: an `OUT_*` handle or a `usize` is an [`OutPort`], an `IN_*` handle or
/// a `usize` an [`InPort`].
pub use reuben_core::operator::{InPort, OutPort};
/// The operator set: constructors and port-index consts, for a host with no document to name a
/// type by string.
pub use reuben_core::operators;
/// Why [`install_graph`] refused a graph, and where it named an undeclared port or a missing node.
pub use reuben_core::plan::{PlanError, PortPosition};
/// The sample rate and block size a Plan is instantiated against — a host's device geometry,
/// which is why it is a parameter rather than a policy.
pub use reuben_core::AudioConfig;

/// Expand an outbound [`Message`]'s single typed [`Arg`] into the flat primitive args an OSC
/// datagram carries, returning `false` when the arg has no OSC form and expanded to nothing.
///
/// The inverse — flat args to one typed `Arg` — is not a host's call: [`RenderSlot::queue_osc`]
/// does it at the destination port, where the type is known.
pub use reuben_core::boundary::osc_out_args;

/// The document-free sibling of `engine::install_initial`: instantiate a Rust-built [`Graph`] and
/// hand back the [`RenderSide`] a callback drives.
///
/// [`swap_pair`]'s coordinator end is minted and dropped here. That is safe because both ends hold
/// an `Arc` of the shared slots, and it costs the swap: the install mailbox stays empty for the
/// life of the slot, so [`RenderSlot::fill`] never ramps and never transplants, and nothing on this
/// feature can fill it (`InstallBundle` is not re-exported). Swapping is the document door's.
///
/// A bare `usize` port past what its node declares — in a wire's source or destination, a master
/// tap, or an `interface` entry — is refused as [`PlanError::PortOutOfRange`]; a typed handle in
/// the wrong position does not compile. A wire, tap or `interface` entry naming a [`NodeKey`] the
/// graph does not hold (left over from a removed node) is refused as [`PlanError::UnknownNode`]. A
/// key minted by another `Graph` is refused only if it names no node here; see [`NodeKey`].
///
/// Allocates; the Instantiate phase, not the audio thread.
pub fn install_graph(graph: Graph, config: AudioConfig) -> Result<RenderSide, PlanError> {
    let plan = reuben_core::plan::Plan::instantiate(graph, config)?;
    let (_coordinator, mailbox) = swap_pair::<reuben_core::coordinator::InstallBundle>();
    Ok(RenderSide {
        engine: reuben_core::engine::Engine::new(plan),
        mailbox,
    })
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use operators::{oscillator, output, Oscillator, Output};

    /// The document-free path end to end, written with nothing but this module's own names, so a
    /// re-export dropped from the render feature fails here rather than in an embedder's repo.
    #[test]
    fn a_rust_built_graph_reaches_audio_through_install_graph() {
        // Neither field matches `AudioConfig::default()`, so the asserts below fail if
        // `install_graph` instantiates against anything but what it was handed.
        let cfg = AudioConfig::new(44_100.0, 64);

        let mut g = Graph::new();
        // Unwired, `osc.freq` materializes 440 Hz from its meta default: no control traffic needed.
        let osc = g.add("/osc", Oscillator::new());
        let out = g.add("/out", Output::new());
        g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
        g.tap_output(out, output::OUT_AUDIO);

        let mut slot = RenderSlot::new(install_graph(g, cfg).expect("instantiate"));
        assert_eq!(slot.sample_rate(), cfg.sample_rate, "rate not from config");
        assert_eq!(slot.block_size(), cfg.block_size, "block not from config");

        let mut buf = vec![0.0f32; cfg.block_size * slot.channels()];
        slot.fill(&mut buf);

        assert!(
            buf.iter().any(|s| *s != 0.0),
            "a 440 Hz oscillator tapped to the master rendered a silent first block"
        );
    }

    /// The refusal half of the signature: Instantiate's `PlanError` reaches an embedder that has
    /// no loader to interpret its graph for it.
    #[test]
    fn a_cyclic_graph_is_refused_as_a_plan_error() {
        let cfg = AudioConfig::new(48_000.0, 256);

        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        let out = g.add("/out", Output::new());
        g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
        g.connect(out, output::OUT_AUDIO, osc, oscillator::IN_FREQ);
        g.tap_output(out, output::OUT_AUDIO);

        // Matched rather than compared: `RenderSide` owns an Engine and implements neither
        // `Debug` nor `PartialEq`, so the `Ok` arm cannot cross an `assert_eq!`.
        match install_graph(g, cfg) {
            Err(e) => assert_eq!(e, PlanError::Cycle),
            Ok(_) => panic!("a cyclic graph instantiated"),
        }
    }

    /// An oscillator and an output, so the port-index tests differ by exactly the wire.
    fn patched(wire: impl FnOnce(&mut Graph, NodeKey, NodeKey)) -> Result<RenderSide, PlanError> {
        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        let out = g.add("/out", Output::new());
        wire(&mut g, osc, out);
        install_graph(g, AudioConfig::new(48_000.0, 64))
    }

    /// A bare source index past the operator's outputs reaches an embedder as a refusal it can
    /// act on, where it used to panic inside Instantiate.
    #[test]
    fn an_out_of_range_source_port_is_refused_as_a_plan_error() {
        let refusal = patched(|g, osc, out| {
            g.connect(osc, 1usize, out, output::IN_AUDIO);
            g.tap_output(out, output::OUT_AUDIO);
        });
        match refusal {
            Err(e) => assert_eq!(
                e,
                PlanError::PortOutOfRange {
                    node: "/osc".into(),
                    position: PortPosition::Source,
                    port: 1,
                    declared: 1,
                }
            ),
            Ok(_) => panic!("an out-of-range source port instantiated"),
        }
    }

    /// The tap position, which reaches the arena by a different path than a wire does.
    #[test]
    fn an_out_of_range_tap_port_is_refused_as_a_plan_error() {
        let refusal = patched(|g, osc, out| {
            g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
            g.tap_output(out, 99usize);
        });
        match refusal {
            Err(e) => assert!(
                matches!(
                    e,
                    PlanError::PortOutOfRange {
                        position: PortPosition::Tap,
                        port: 99,
                        ..
                    }
                ),
                "{e:?}"
            ),
            Ok(_) => panic!("an out-of-range tap port instantiated"),
        }
    }

    /// The destination position is refused too, rather than planning a wire that renders nothing;
    /// and as the port, not as the cycle its back edge would otherwise close.
    #[test]
    fn an_out_of_range_destination_port_is_refused_as_a_plan_error() {
        for back_edge in [false, true] {
            let refusal = patched(|g, osc, out| {
                g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
                if back_edge {
                    g.connect(out, output::OUT_AUDIO, osc, 99usize);
                } else {
                    g.connect(osc, oscillator::OUT_AUDIO, out, 99usize);
                }
                g.tap_output(out, output::OUT_AUDIO);
            });
            match refusal {
                Err(e) => assert!(
                    matches!(
                        e,
                        PlanError::PortOutOfRange {
                            position: PortPosition::Destination,
                            port: 99,
                            ..
                        }
                    ),
                    "back edge {back_edge}: {e:?}"
                ),
                Ok(_) => panic!("an out-of-range destination port instantiated"),
            }
        }
    }

    /// `Graph::interface` is public, so its ports are an embedder's to get wrong as well.
    #[test]
    fn an_out_of_range_interface_output_port_is_refused_as_a_plan_error() {
        let refusal = patched(|g, osc, out| {
            g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
            g.tap_output(out, output::OUT_AUDIO);
            g.interface.outputs.insert("audio".into(), (out, 7));
        });
        match refusal {
            Err(e) => assert!(
                matches!(
                    &e,
                    PlanError::PortOutOfRange {
                        position: PortPosition::InterfaceOutput(name),
                        port: 7,
                        ..
                    } if name == "audio"
                ),
                "{e:?}"
            ),
            Ok(_) => panic!("an out-of-range interface output port instantiated"),
        }
    }

    /// Adds a node, then removes it, leaving `wire` a key the graph no longer holds.
    fn with_stale_key(
        wire: impl FnOnce(&mut Graph, NodeKey, NodeKey, NodeKey),
    ) -> Result<RenderSide, PlanError> {
        patched(|g, osc, out| {
            let gone = g.add("/gone", Oscillator::new());
            g.nodes.remove(gone);
            wire(g, osc, out, gone);
        })
    }

    fn assert_unknown_node(refusal: Result<RenderSide, PlanError>, expected: PortPosition) {
        match refusal {
            Err(e) => assert_eq!(e, PlanError::UnknownNode { position: expected }),
            Ok(_) => panic!("a graph naming a missing node instantiated"),
        }
    }

    #[test]
    fn a_wire_from_a_missing_node_is_refused_as_a_plan_error() {
        let refusal = with_stale_key(|g, _, out, gone| {
            g.connect(gone, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
            g.tap_output(out, output::OUT_AUDIO);
        });
        assert_unknown_node(refusal, PortPosition::Source);
    }

    #[test]
    fn a_wire_into_a_missing_node_is_refused_as_a_plan_error() {
        let refusal = with_stale_key(|g, osc, out, gone| {
            g.connect(osc, oscillator::OUT_AUDIO, gone, output::IN_AUDIO);
            g.tap_output(out, output::OUT_AUDIO);
        });
        assert_unknown_node(refusal, PortPosition::Destination);
    }

    #[test]
    fn a_tap_on_a_missing_node_is_refused_as_a_plan_error() {
        let refusal = with_stale_key(|g, osc, out, gone| {
            g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
            g.tap_output(gone, output::OUT_AUDIO);
        });
        assert_unknown_node(refusal, PortPosition::Tap);
    }

    #[test]
    fn an_interface_input_on_a_missing_node_is_refused_as_a_plan_error() {
        let refusal = with_stale_key(|g, osc, out, gone| {
            g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
            g.tap_output(out, output::OUT_AUDIO);
            g.interface.inputs.insert("freq".into(), (gone, 0));
        });
        assert_unknown_node(refusal, PortPosition::InterfaceInput("freq".into()));
    }

    #[test]
    fn an_interface_output_on_a_missing_node_is_refused_as_a_plan_error() {
        let refusal = with_stale_key(|g, osc, out, gone| {
            g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
            g.tap_output(out, output::OUT_AUDIO);
            g.interface.outputs.insert("audio".into(), (gone, 0));
        });
        assert_unknown_node(refusal, PortPosition::InterfaceOutput("audio".into()));
    }
}
