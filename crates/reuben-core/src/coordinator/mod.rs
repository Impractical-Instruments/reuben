//! The Coordinator region's RT boundary machinery: [`mailbox`] is the primitive; [`install`],
//! [`migration`] and [`slot`] build on it. [`retire`] is the per-value counterpart of the mailbox's
//! retire slot.
//!
//! The off-thread half — the `Coordinator` that loads a document, diffs its manifest and posts the
//! install — is `reuben-document`'s. What stays here is everything the audio callback touches, and
//! it names no document type: a bundle carries a built [`Engine`](crate::engine::Engine) and a flat
//! table of survivor index pairs, never the instrument that produced them.
//!
//! The engine channel's NDJSON envelope used to live here beside them. It does not: both of its
//! ends are doors, so it is the window's — nothing in this crate serializes it.

pub mod install;
pub mod mailbox;
pub mod migration;
pub mod retire;
pub mod slot;

pub use install::{InstallBundle, RenderSide};
pub use mailbox::{
    swap_pair, CoordinatorMailbox, ReclaimError, RenderMailbox, SwapInFlight, SwapTimeout,
};
pub use migration::MigrationTable;
pub use retire::{retire_queue, OwnsHeap, RetireDrain, RetireQueue};
pub use slot::RenderSlot;
