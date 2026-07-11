//! The Coordinator region's RT boundary machinery (ADR-0046, ADR-0009, ADR-0012).
//!
//! This module is the non-RT side of the Swap lifecycle: the Coordinator is the single
//! writer of graph structure (ADR-0012), and everything it hands the render side crosses
//! lock-free. Today it holds two primitives: the [`mailbox`] pair — the single-slot atomic
//! channel a Swap rides (ADR-0046 §2) — and [`wire`], the structure channel's shared NDJSON
//! `Request`/`Response` envelope (ADR-0046 §8), serialized identically by the native server
//! and the reuben-mcp client. The Coordinator struct itself (manifest, fingerprints,
//! migration table, `swap_document`) lands in later tickets on top of these primitives.
//!
//! Like the rest of reuben-core, this module is OS-free: no clock, no threads, no I/O.
//!
//! ADR-0046's M2 pieces live here on top of the primitives: [`manifest`] (the installed-Plan
//! manifest, the survivor-key fingerprint, and the migration table, §§4,5), [`swap`] (the
//! passive [`Coordinator`] struct, §7 — `swap_document` builds a whole new Engine off-thread,
//! precomputes the migration table, and fills the install mailbox; single-writer via `&mut self`),
//! and [`slot`] (the RT counterpart, §7 — the [`RenderSlot`] each shell drives instead of calling
//! `Engine::fill` directly: it drains the install mailbox, runs ADR-0050's master-gain ramp,
//! box-transplants the survivors, and posts the retiree, all allocation-/lock-/drop-free).

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
pub use wire::{DiagnosticsReport, DocSource, Request, Response, DEFAULT_STRUCTURE_ADDR};
