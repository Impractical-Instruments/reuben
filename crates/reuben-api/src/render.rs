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
/// The operator set: constructors and port-index consts, for a host with no document to name a
/// type by string.
pub use reuben_core::operators;
/// Why [`install_graph`] refused a graph. Not the whole failure surface — see [`install_graph`].
pub use reuben_core::plan::PlanError;
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
/// [`PlanError`] is not the whole failure surface: an out-of-range port index panics inside
/// Instantiate, and the port positions disagree about it; the tests pin what each does.
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

    /// A source port index past the operator's outputs panics inside Instantiate instead of
    /// returning [`PlanError`] — `connect` takes `impl PortIndex` in both positions, so an input
    /// handle is accepted there. Known defect; fixing it should fail this test.
    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn an_out_of_range_source_port_panics_instead_of_refusing() {
        let _ = patched(|g, osc, out| {
            g.connect(osc, oscillator::IN_WAVEFORM, out, output::IN_AUDIO);
            g.tap_output(out, output::OUT_AUDIO);
        });
    }

    /// The same panic from the tap position, which reaches it by a different path than a wire
    /// does. Known defect; fixing it should fail this test.
    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn an_out_of_range_tap_port_panics_instead_of_refusing() {
        let _ = patched(|g, osc, out| {
            g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
            g.tap_output(out, 99);
        });
    }

    /// A destination port index past the operator's inputs does not panic: it plans, and the wire
    /// it dropped renders as silence. Known defect; fixing it should fail this test.
    #[test]
    fn an_out_of_range_destination_port_plans_and_renders_silence() {
        let side = patched(|g, osc, out| {
            g.connect(osc, oscillator::OUT_AUDIO, out, 99);
            g.tap_output(out, output::OUT_AUDIO);
        })
        .expect("an out-of-range destination port did not refuse the graph");

        let mut slot = RenderSlot::new(side);
        let mut buf = vec![0.0f32; slot.block_size() * slot.channels()];
        slot.fill(&mut buf);

        assert!(
            buf.iter().all(|s| *s == 0.0),
            "the dropped wire fed the master anyway"
        );
    }

    /// A dropped destination wire is dropped as a *data* path only. It survives as a topological
    /// edge, so it still closes a cycle and still constrains evaluation order. Known defect; fixing
    /// it should fail this test.
    #[test]
    fn an_out_of_range_destination_port_still_closes_a_cycle() {
        let refusal = patched(|g, osc, out| {
            g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
            g.connect(out, output::OUT_AUDIO, osc, 99);
            g.tap_output(out, output::OUT_AUDIO);
        });

        match refusal {
            Err(e) => assert_eq!(e, PlanError::Cycle),
            Ok(_) => panic!("the out-of-range back edge left the graph acyclic"),
        }
    }
}
