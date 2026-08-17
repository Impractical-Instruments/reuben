//! The render surface: what a host drives per block, however it likes — a tight loop for offline
//! render, a device callback, a Web Audio render callback.
//!
//! Nothing here converts. A handle that crosses this boundary is re-exported or passed through
//! inline, because anything else is paid once per block and shows up as an instruction-count
//! regression rather than as a design opinion.
//!
//! The shape a host wires up is the same everywhere: something builds a [`RenderSide`], the
//! callback wraps it in a [`RenderSlot`] and calls [`fill`](RenderSlot::fill) or
//! [`fill_duplex`](RenderSlot::fill_duplex). Everything a swap does — the install mailbox, the
//! survivor transplant, the master-gain ramp — happens inside those two calls, so a host never
//! sequences it.
//!
//! **What differs between hosts is graph construction, not rendering, and this module carries only
//! the document-free way in.** [`install_graph`] takes a [`Graph`] built in Rust — no document, no
//! serde, no filesystem — and hands back the render side. The document-loading route,
//! `engine::install_initial`, needs `authoring` **and** `render` — it takes a document through the
//! loader and hands back a render side, so a build with either feature alone does not have it —
//! and additionally returns the off-thread `Coordinator` (the single writer of graph structure,
//! which the engine verbs then drive) and the load warnings a resolver produced. A host that
//! compiles both drives the same [`RenderSlot`] either way.
//!
//! **One handle reaches a host that this module never names.** [`RenderSide`] holds an `Engine` in
//! a `pub` field, so [`install_graph`] hands one over whether or not the type is nameable here —
//! `side.engine.fill(…)` and the rest of its API come with it. Nothing render-only could construct
//! a `RenderSide` before this door existed, which is why that surface was uninhabited rather than
//! closed. What keeps a host on [`RenderSlot`] is convention, not the type system; what it gets for
//! staying is the install mailbox and the master-gain ramp, which a bare `Engine` has neither of.
//!
//! **`Registry` is kept out on purpose, and that one is a size decision.** A Rust-built graph names
//! an operator type by its constructor, and `Registry::builtin()` flattens a `const` census that
//! names every operator's `make`/`descriptor` fn pointer — so exposing it is what would pull the
//! whole operator set into an image where size binds. Re-exporting [`mod@operators`] costs nothing
//! by contrast: a `pub use` re-exports names, and only the operators a graph actually constructs
//! are codegen'd. Adding an export later is non-breaking; un-exposing one is not.

/// The render side of a fresh pair — the initial Engine plus the mailbox a swap installs through.
/// [`RenderSlot::new`] takes it; nothing else does.
pub use reuben_core::coordinator::RenderSide;
/// The RT-side handle. Every per-block call a host makes is one of its methods.
pub use reuben_core::coordinator::RenderSlot;
/// The single-slot atomic swap channel underneath the install mailbox, re-exported as the
/// **primitive** it is: a host with its own RT-side payload (the native door's device output map)
/// builds a parallel channel out of it rather than inventing a second lock-free mailbox.
pub use reuben_core::coordinator::{swap_pair, CoordinatorMailbox, RenderMailbox, SwapInFlight};
/// The graph a host builds in Rust, and the handle each `add` hands back for wiring. This is the
/// construction vocabulary [`install_graph`] consumes; the operators it is built out of are
/// [`mod@operators`].
pub use reuben_core::graph::{Graph, NodeKey};
/// One control atom and the internal message carrying it. A host mints [`Arg`]s at its own foreign
/// edge (an OSC datagram, a MIDI event) and hands them to [`RenderSlot::queue_osc`]; a [`Message`]
/// is what comes back out of [`RenderSlot::drain_outbound`].
pub use reuben_core::message::{Arg, Message};
/// The operator set, constructors and port-index consts alike — what a [`Graph`] is built out of
/// when there is no document to name a type by string.
pub use reuben_core::operators;
/// Why [`install_graph`] *refused* the graph — and not everything that can go wrong with one; see
/// [`install_graph`].
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
/// Nothing here produces the document door's extra returns: no resources were resolved (no load
/// warnings), a graph handed over by value has no later swap to write (no `Coordinator`), and a
/// constructor names its own operator type (no registry to look a string up in).
///
/// **Dropping that Coordinator end is safe** — both ends hold an `Arc` of the shared slots, so the
/// render end is whole without it. **What it costs is a swap**: the install mailbox stays empty for
/// the life of the slot, so [`RenderSlot::fill`] never ramps and never transplants, and nothing on
/// this feature can fill it (`InstallBundle` is not re-exported). A host that needs to swap takes
/// the document door.
///
/// **[`PlanError`] is not the whole failure surface** — an out-of-range port index panics inside
/// Instantiate rather than arriving here, and the three port positions do not agree about it. The
/// tests in this module pin what each one does.
///
/// Allocates, and is not for the audio thread: it is the Instantiate phase, paid at setup.
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

    /// The document-free path end to end, and deliberately written with nothing but this module's
    /// own names: a `Graph` built in Rust, `install_graph`, `RenderSlot::new`, `fill`. It is the
    /// walkthrough the bare-metal embedder follows, so a re-export dropped from the render feature
    /// fails here rather than in someone else's repository.
    #[test]
    fn a_rust_built_graph_reaches_audio_through_install_graph() {
        // Neither field matches `AudioConfig::default()` (48 kHz / 128), so the geometry asserts
        // below fail if `install_graph` ever instantiates against anything but what it was handed.
        // Non-silence alone would not notice: `fill_duplex` is chunk-size independent, and a sine
        // is a sine at any rate.
        let cfg = AudioConfig::new(44_100.0, 64);

        let mut g = Graph::new();
        // `osc.freq` is left unwired — it materializes 440 Hz from its meta default, so the graph
        // needs no control traffic to make sound.
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

    /// The refusal half of the signature: `install_graph` answers with the same `PlanError`
    /// Instantiate raises, so an embedder with no loader still learns why its graph was rejected.
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

    /// An oscillator and an output, wired by a closure so the four port-index tests below differ
    /// by exactly the wire under test.
    fn patched(wire: impl FnOnce(&mut Graph, NodeKey, NodeKey)) -> Result<RenderSide, PlanError> {
        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        let out = g.add("/out", Output::new());
        wire(&mut g, osc, out);
        install_graph(g, AudioConfig::new(48_000.0, 64))
    }

    /// Pins current behaviour that is a known defect, tracked as reuben#770: a **source** port
    /// index outside the operator's outputs panics inside Instantiate instead of returning
    /// [`PlanError`]. `IN_WAVEFORM` is an input handle that `connect` accepts in the source
    /// position, and the oscillator declares one output. Whoever fixes #770 should expect this
    /// test to fail — it is the change-detector, not something they broke.
    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn an_out_of_range_source_port_panics_instead_of_refusing() {
        let _ = patched(|g, osc, out| {
            g.connect(osc, oscillator::IN_WAVEFORM, out, output::IN_AUDIO);
            g.tap_output(out, output::OUT_AUDIO);
        });
    }

    /// Pins current behaviour that is a known defect, tracked as reuben#770: same panic from the
    /// **tap** position, which reaches it by a different path than the wire above.
    #[test]
    #[should_panic(expected = "index out of bounds")]
    fn an_out_of_range_tap_port_panics_instead_of_refusing() {
        let _ = patched(|g, osc, out| {
            g.connect(osc, oscillator::OUT_AUDIO, out, output::IN_AUDIO);
            g.tap_output(out, oscillator::IN_WAVEFORM);
        });
    }

    /// Pins current behaviour that is a known defect, tracked as reuben#770: the **destination**
    /// position does not panic — it plans, and the wire it dropped renders as silence. The
    /// asymmetry against the two tests above is the defect, not this test's expectation.
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

    /// Pins current behaviour that is a known defect, tracked as reuben#770, and the half of it
    /// that is easy to miss: a dropped destination wire is dropped as a *data* path only. It
    /// survives as a topological edge, so a back edge into an out-of-range destination port still
    /// closes a cycle — which means such a wire silently constrains evaluation order too.
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
