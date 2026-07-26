//! The Coordinator region's RT boundary machinery: [`mailbox`] (the single-slot atomic swap
//! channel) is the primitive; [`manifest`] (survivor fingerprint + migration table), [`swap`] (the
//! off-thread [`Coordinator`]), and [`slot`] (the RT-side [`RenderSlot`]) build on it.
//!
//! The structure channel's NDJSON envelope used to live here beside them. It does not: both of its
//! ends are doors, so it is the window's — nothing in this crate serializes it.
//!
//! see rules: execution-runtime

pub mod mailbox;
pub mod manifest;
pub mod slot;
pub mod swap;

pub use mailbox::{
    swap_pair, CoordinatorMailbox, ReclaimError, RenderMailbox, SwapInFlight, SwapTimeout,
};
pub use manifest::{build_manifest, Manifest, MigrationTable, NodeIdentity};
pub use slot::RenderSlot;
pub use swap::{Coordinator, InstallBundle, RenderSide};
