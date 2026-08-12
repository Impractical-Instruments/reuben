//! reuben-document — authoring instrument documents, and turning them into an engine Plan.
//!
//! The instrument format and its loader ([`format`]), the edit verbs ([`edit`]), the agent's view of
//! a document ([`projection`], [`introspect`], [`describe`]), the report shapes those produce
//! ([`contract`]), the serde-shaped intent vocabulary ([`vocabulary`]), the resource seam every one
//! of them resolves through ([`resources`]), and the off-thread [`coordinator`] that installs a
//! loaded document into a running render side.
//!
//! **Everything here sits above `reuben-core` and imports upward.** `reuben-core` is the render
//! half — it holds the mechanics of rendering audio and names nothing in this crate, which is what
//! lets it compile for a target that has no filesystem and no serde. A type that the audio callback
//! touches belongs below, even when the thing that builds it lives here: `Manifest::diff` is the
//! clearest case, producing a [`reuben_core::coordinator::MigrationTable`] the RT slot applies.
//!
//! see rules: execution-runtime

pub mod contract;
pub mod coordinator;
pub mod describe;
pub mod edit;
pub mod engine;
pub mod format;
pub mod introspect;
pub mod projection;
pub mod resources;
pub mod vocabulary;

pub use contract::{content_hash, Diag, DiffSummary, Report, SwapReport};
pub use coordinator::{Coordinator, PreparedSwap};
pub use describe::{describe_boundary, BoundaryDesc, BoundaryPortDesc};
pub use engine::{from_document, FromDocumentError};
pub use format::{
    load, load_instrument, load_instrument_doc, resolve_instrument, DocValue, InstrumentDoc,
    InterfaceDoc, LoadError, LoadWarning, Loaded, NormalizedDoc, SCAFFOLD_DEFAULT_NAME,
};
pub use resources::{MemoryResolver, ResolveError, ResourceResolver};
