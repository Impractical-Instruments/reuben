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

pub mod mailbox;
pub mod wire;

pub use mailbox::{
    swap_pair, CoordinatorMailbox, ReclaimError, RenderMailbox, SwapInFlight, SwapTimeout,
};
pub use wire::{DiagnosticsReport, DocSource, Request, Response};
