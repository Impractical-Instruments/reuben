//! The off-thread half of the Coordinator region. The RT half is
//! [`reuben_core::coordinator`]; the seam runs between deciding *what* survives — which needs the
//! document — and *applying* it, which must not.
//!
//! see rules: execution-runtime

pub mod manifest;
pub mod swap;

pub use manifest::{build_manifest, Manifest, NodeIdentity};
pub use swap::{Coordinator, PreparedSwap};
