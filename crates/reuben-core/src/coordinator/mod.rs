//! The Coordinator region's RT boundary machinery: [`mailbox`] is the primitive; [`install`],
//! [`migration`] and [`slot`] build on it.
//!
//! The off-thread half — the `Coordinator` that loads a document, diffs its manifest and posts the
//! install — is `reuben-document`'s. What stays here is everything the audio callback touches, and
//! it names no document type: a bundle carries a built [`Engine`](crate::engine::Engine) and a flat
//! table of survivor index pairs, never the instrument that produced them.
//!
//! The structure channel's NDJSON envelope used to live here beside them. It does not: both of its
//! ends are doors, so it is the window's — nothing in this crate serializes it.
//!
//! see rules: execution-runtime

pub mod install;
pub mod mailbox;
pub mod migration;
pub mod slot;

pub use install::{InstallBundle, RenderSide};
pub use mailbox::{
    swap_pair, CoordinatorMailbox, ReclaimError, RenderMailbox, SwapInFlight, SwapTimeout,
};
pub use migration::MigrationTable;
pub use slot::RenderSlot;
