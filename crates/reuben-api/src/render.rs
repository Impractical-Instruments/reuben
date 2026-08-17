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
//! **What is deliberately not here**: `Plan`, `Registry`, `Coordinator`, and `Renderer`'s
//! `render_block` family — [`install_graph`] is the only way through.
//!
//! **`Engine` is the exception, and naming it here is the honest thing to do.** [`RenderSide`]
//! holds it in a `pub` field, so the door hands one over whether or not the type is nameable on
//! this feature: `side.engine.fill(…)`, `queue_osc`, `drain_outbound` and the rest are all
//! reachable through it. Nothing render-only could construct a `RenderSide` before this door
//! existed, which is why that surface was uninhabited rather than closed. What keeps a host on
//! [`RenderSlot`] is convention, not the type system — and what it gets for staying is the install
//! mailbox and the master-gain ramp, which a bare `Engine` has neither of.
//!
//! `Registry` is the one worth a paragraph of its own: a Rust-built graph names an operator type by
//! its constructor, and `Registry::builtin()` flattens a `const` census that names every operator's
//! `make`/`descriptor` fn pointer, so exposing it is what would pull the whole operator set into an
//! image where size binds. Re-exporting [`mod@operators`] costs nothing by contrast — a `pub use`
//! re-exports names, and only the operators a graph actually constructs are codegen'd. Adding an
//! export later is non-breaking; un-exposing one is not.

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
/// Why [`install_graph`] *refused* the graph: a cycle, or two wire ends whose forms cannot
/// connect. Those two variants are the whole enum, and the enum is not the whole failure surface —
/// [`install_graph`] names the port index that panics instead of arriving here.
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
/// Set against the document door this drops three things, each because there is nothing behind it
/// to carry: the `Coordinator` (a graph handed over by value has no later swap to write), the load
/// warnings (no resources were resolved), and the registry (a constructor names its operator type,
/// so no string ever has to be looked up).
///
/// The pair's Coordinator end is built and dropped here rather than never built: [`swap_pair`] is
/// what mints the shared slots, and both ends hold an `Arc` of them, so dropping one leaves the
/// render end whole. What it costs is a swap — the install mailbox stays empty for the life of the
/// slot, so [`RenderSlot::fill`] never ramps and never transplants.
///
/// **There is no render-only way to get that swap back**, and the narrowness is deliberate:
/// `InstallBundle`, the payload the install mailbox carries, is not re-exported on this feature, so
/// nothing here can fill one. A host that needs to swap takes the document door. ([`swap_pair`] is
/// re-exported for a host's *own* RT-side payload — the native door's device output map — not for a
/// second engine install.)
///
/// **[`PlanError`] is not the whole failure surface: a port index outside the operator's declared
/// ports panics inside Instantiate rather than coming back here.** The handles are typed and a port
/// carries its direction in its type — `oscillator::OUT_AUDIO` is an `Out<SignalF32>`,
/// `oscillator::IN_WAVEFORM` an `In<Held<Waveform>>` — but [`Graph::connect`] takes
/// `impl PortIndex` in *both* port positions, and [`Graph::tap_output`] in its one, so a handle
/// pointing the wrong way type-checks there. `IN_WAVEFORM`'s ordinal is `1`, the oscillator
/// declares one output, and Instantiate indexes the outputs list with it.
///
/// It is not uniform across the three positions. An out-of-range **source** port and an
/// out-of-range **tap** port both panic. An out-of-range **destination** port plans `Ok` and drops
/// the wire as a *data* path — but it survives as an *ordering* edge, because the topological sort
/// counts every connection while only the data wiring checks the port against the descriptor. So it
/// still constrains evaluation order, and a back edge into one still comes back as
/// [`PlanError::Cycle`].
///
/// What avoids all of it is passing the const whose direction matches the position: an `OUT_*` in a
/// source or tap position, an `IN_*` in a destination one. A bare `usize` also satisfies the bound,
/// but it is the escape hatch for computed ports and the loader's resolved ordinals rather than the
/// wiring vocabulary.
///
/// This is worth more attention on a bare-metal target than the `Result` is — there, a panic is the
/// embedder's `panic_handler`, which is a dark board rather than an error it can read.
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
}
