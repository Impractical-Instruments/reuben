//! The render surface: what a host drives per block, however it likes — a tight loop for offline
//! render, a device callback, a Web Audio render callback.
//!
//! Nothing here converts. A handle that crosses this boundary is re-exported or passed through
//! inline, because anything else is paid once per block and shows up as an instruction-count
//! regression rather than as a design opinion.
//!
//! The shape a host wires up is a **pair**: [`install_initial`] builds the off-thread
//! [`Coordinator`] (the single writer of graph structure, which the engine verbs then drive) and
//! the [`RenderSide`] its callback owns. The callback wraps that side in a [`RenderSlot`] and calls
//! [`fill`](RenderSlot::fill) or [`fill_duplex`](RenderSlot::fill_duplex); everything a swap does
//! — the install mailbox, the survivor transplant, the master-gain ramp — happens inside those two
//! calls, so a host never sequences it.
//!
//! **The one call that is not a re-export is the one that is not on a block.** `install_initial`
//! hides the registry (there is one operator set, and a host that had to name it would be naming
//! the engine to get its own constructor) and takes the window's own [`Resources`] seam, so a host
//! implements one resolver trait rather than two. Both cost a load, not a block.

/// The render side of a fresh pair — the initial Engine plus the mailbox a swap installs through.
/// [`RenderSlot::new`] takes it; nothing else does.
pub use reuben_core::coordinator::RenderSide;
/// The RT-side handle. Every per-block call a host makes is one of its methods.
pub use reuben_core::coordinator::RenderSlot;
/// The single-slot atomic swap channel underneath the install mailbox, re-exported as the
/// **primitive** it is: a host with its own RT-side payload (the native door's device output map)
/// builds a parallel channel out of it rather than inventing a second lock-free mailbox.
pub use reuben_core::coordinator::{swap_pair, CoordinatorMailbox, RenderMailbox, SwapInFlight};
/// One control atom and the internal message carrying it. A host mints [`Arg`]s at its own foreign
/// edge (an OSC datagram, a MIDI event) and hands them to [`RenderSlot::queue_osc`]; a [`Message`]
/// is what comes back out of [`RenderSlot::drain_outbound`].
pub use reuben_core::message::{Arg, Message};
/// The sample rate and block size a Plan is instantiated against — a host's device geometry,
/// which is why it is a parameter rather than a policy.
pub use reuben_core::AudioConfig;

/// Expand an outbound [`Message`]'s single typed [`Arg`] into the flat primitive args an OSC
/// datagram carries, returning `false` when the arg has no OSC form and expanded to nothing.
///
/// The inverse — flat args to one typed `Arg` — is not a host's call: [`RenderSlot::queue_osc`]
/// does it at the destination port, where the type is known.
pub use reuben_core::boundary::osc_out_args;
