//! The authoring surface: the document verbs, the introspection reads, and the result shapes a
//! door advertises — flat at the window, because flat is what MCP and generated wrappers want,
//! whatever shape the internals prefer.
//!
//! A door's whole job here is to turn its own transport into an argument struct, hand it a
//! [`Resources`] store, and render the [`Answer`] or [`Refusal`] that comes back. Everything
//! between — the registry, the `expect` guard, the validation pipeline, the projection echo, the
//! one-line gloss — is the window's.
//!
//! The types a door serializes are the window's own, not the engine's; the two types crossing the
//! resource seam are the engine's, re-exported. [`crate::resources`] states that split and why.
//!
//! see rules: agent-mcp

mod args;
pub mod prose;
mod result;
mod verbs;

/// The resource seam, which is neither half's: a door that only drives this one still names it
/// here. [`crate::resources`] holds it, because the render half calls the same trait.
pub use crate::resources::{ResolveError, Resources, SampleBuffer};
pub use args::*;
pub use result::{
    Boundary, Diag, DocumentView, EditResult, OperatorInfo, Operators, PortInfo, Report,
};
pub use verbs::*;

/// The notation key for the compact operator listing, so a door shipping that listing as
/// standalone grounding can say what its punctuation means. The engine's, re-exported rather than
/// restated: a second copy of a legend for a generated notation is a second copy that can describe
/// the notation wrongly.
pub use reuben_core::introspect::COMPACT_DESCRIBE_LEGEND;

/// The instrument document `format_version` this window loads and writes — what a door reports
/// when a caller asks which format it speaks. The engine's constant, re-exported: a value, not a
/// type.
pub use reuben_core::format::FORMAT_VERSION;

/// The `instrument` name a new document gets when its author does not choose one — what a door
/// fills [`NewInstrument::name`] with for a caller that omitted it. A value, not a type: the
/// engine's constant is the one authority on it.
pub use reuben_core::SCAFFOLD_DEFAULT_NAME;
