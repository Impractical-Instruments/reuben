//! The engine half: reaching a **running** engine — install a document, audition a control
//! gesture, ask what is playing, read the counters.
//!
//! "Engine" here is the running instance a door reaches, not the render Engine a host drives per
//! block; that one is [`crate::render`]'s. Both ends of the reach are doors — `reuben play` serves
//! the structure channel, the MCP sidecar dials it — which is why the envelope, the verbs and the
//! guards are the window's rather than either door's.
//!
//! The three pieces, and which side of the channel each serves:
//!
//! - [`wire`] — what both ends serialize. Neither door owns it, so neither can move it alone.
//! - [`verbs`](self) over a [`Channel`] — the client side. A door supplies a [`Transport`]
//!   (sockets, timeouts, its own reconnect policy) and gets the framing, the response
//!   classification, the result shapes and the glosses.
//! - [`server`] — the serving side, over an [`EngineHost`] seam. A host supplies its device map,
//!   its control ingress, its counters and the clock its deferred free waits on; the window
//!   decides what each verb *means*, the expect guard included.
//!
//! This module is compiled with the `authoring` feature: it is the control half of the
//! authoring/control side of the window — off-thread, serialized, and never on a block.

mod args;
mod channel;
pub mod prose;
mod result;
pub mod server;
mod verbs;
pub mod wire;

pub use args::*;
pub use channel::{Channel, ChannelError, SwapOutcome, Transport};
pub use prose::ENGINE_UNREACHABLE_GUIDANCE;
pub use result::{
    CurrentInstrument, EngineStatus, SendOutput, SidecarInfo, StatusEndpoints, SwapResult,
};
pub use server::{dispatch, EngineHost, IngressClosed, StructureState};

/// The single-writer graph owner an [`StructureState`] serves. The engine's type, re-exported because
/// a host has to name it to build one — and reaching past the window for the *one* type its own
/// constructor demands would make "every consumer goes through the window" false for every host
/// there will ever be.
pub use reuben_document::coordinator::Coordinator;
/// A built-but-not-installed swap: what `Coordinator::prepare_document` returns, so a host whose
/// render buffers are a fixed size reads the built Engine's geometry and either commits it or drops
/// it. Re-exported because a host that holds one between the two calls has to name its type.
pub use reuben_document::coordinator::PreparedSwap;
/// A non-fatal resource problem from a load — a missing sample degrades to silence and says so.
pub use reuben_document::format::LoadWarning;
/// Why an initial install failed: the document did not load, or it loaded and would not plan.
pub use reuben_document::FromDocumentError;
pub use verbs::*;
pub use wire::{
    Conflict, ControlArg, ControlMessage, DiagnosticsReport, DiffSummary, DocSource,
    DocumentSnapshot, Request, Response, SwapReport, DEFAULT_STRUCTURE_ADDR, MAX_SEND_BATCH,
};

/// Build the [`Coordinator`] + [`RenderSide`](crate::render::RenderSide) pair for `doc_json`, at
/// `config`, resolving the document's samples and nested children through `resources`.
///
/// The initial Engine is installed *directly* into the render side — it does not cross the install
/// mailbox — so the first swap is what first fills it. Resource problems are non-fatal and come
/// back as [`LoadWarning`]s for the host to surface; only a document that will not load or will
/// not plan is an error.
///
/// It sits on the authoring surface because its *input* is a document, and everything it touches to
/// get from that string to a built Engine is `reuben-document`'s. What it hands back is the render
/// surface's, and a host drives that half from its callback.
#[cfg(feature = "render")]
pub fn install_initial<R: crate::resources::Resources + Send + 'static>(
    doc_json: &str,
    resources: R,
    config: reuben_core::AudioConfig,
) -> Result<(Coordinator, crate::render::RenderSide, Vec<LoadWarning>), FromDocumentError> {
    Coordinator::install_initial(
        doc_json,
        reuben_core::Registry::builtin(),
        Box::new(crate::resources::OwnedAdapter(resources)),
        config,
    )
}
