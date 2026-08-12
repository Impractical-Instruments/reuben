//! The off-thread half of the Coordinator region: [`swap`] (the single-writer [`Coordinator`] that
//! loads a document and posts an install) and [`manifest`] (the survivor fingerprints it diffs to
//! decide what may be transplanted).
//!
//! The RT half is `reuben-core`'s [`coordinator`](reuben_core::coordinator): the mailbox primitive,
//! the install payload, the migration table and the `RenderSlot` that drains them. The seam runs
//! between deciding *what* survives — which needs the document — and *applying* it, which must not.
//!
//! see rules: execution-runtime

pub mod manifest;
pub mod swap;

pub use manifest::{build_manifest, Manifest, NodeIdentity};
pub use swap::{Coordinator, PreparedSwap};
