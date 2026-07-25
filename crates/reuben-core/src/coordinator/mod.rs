//! The Coordinator region's RT boundary machinery: [`mailbox`] (the single-slot atomic swap
//! channel) and [`wire`] (the NDJSON `Request`/`Response` envelope shared by the native server
//! and the reuben-mcp client) are the primitives; [`manifest`] (survivor fingerprint + migration
//! table), [`swap`] (the off-thread [`Coordinator`]), and [`slot`] (the RT-side [`RenderSlot`])
//! build on them.
//!
//! see rules: execution-runtime

pub mod mailbox;
pub mod manifest;
pub mod slot;
pub mod swap;
pub mod wire;

pub use mailbox::{
    swap_pair, CoordinatorMailbox, ReclaimError, RenderMailbox, SwapInFlight, SwapTimeout,
};
pub use manifest::{build_manifest, Manifest, MigrationTable, NodeIdentity};
pub use slot::RenderSlot;
pub use swap::{Coordinator, InstallBundle, RenderSide};
pub use wire::{
    Conflict, ControlArg, ControlMessage, DiagnosticsReport, DocSource, DocumentSnapshot, Request,
    Response, DEFAULT_STRUCTURE_ADDR, MAX_SEND_BATCH,
};
