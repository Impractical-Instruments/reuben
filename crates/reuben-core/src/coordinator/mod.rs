//! The Coordinator region's RT boundary machinery.
//!
//! This module is the non-RT side of the Swap lifecycle: the Coordinator is the single
//! writer of graph structure, and everything it hands the render side crosses
//! lock-free. Two primitives sit underneath: the [`mailbox`] pair — the single-slot atomic
//! channel a Swap rides — and [`wire`], the structure channel's shared NDJSON
//! `Request`/`Response` envelope, serialized identically by the native server
//! and the reuben-mcp client.
//!
//! Like the rest of reuben-core, this module is OS-free: no clock, no threads, no I/O.
//!
//! The Coordinator's higher-level pieces live here on top of the primitives: [`manifest`] (the installed-Plan
//! manifest, the survivor-key fingerprint, and the migration table), [`swap`] (the
//! passive [`Coordinator`] struct — `swap_document` builds a whole new Engine off-thread,
//! precomputes the migration table, and fills the install mailbox; single-writer via `&mut self`),
//! and [`slot`] (the RT counterpart — the [`RenderSlot`] each shell drives instead of calling
//! `Engine::fill` directly: it drains the install mailbox, runs the master-gain ramp,
//! box-transplants the survivors, and posts the retiree, all allocation-/lock-/drop-free).
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
