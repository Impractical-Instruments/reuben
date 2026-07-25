//! The authoring surface: the document verbs, the introspection reads, and the result shapes a
//! door advertises — flat at the window, because flat is what MCP and generated wrappers want,
//! whatever shape the internals prefer.
//!
//! A door's whole job here is to turn its own transport into an argument struct, hand it a
//! [`Resources`] store, and render the [`Answer`] or [`Refusal`] that comes back. Everything
//! between — the registry, the `expect` guard, the validation pipeline, the projection echo, the
//! one-line gloss — is the window's.
//!
//! The types are the window's own, not the engine's: nothing here is a projection of what
//! `reuben_core` happens to expose, so the engine stays free to pick representations serde is bad
//! at. The cost is a conversion layer, which is real code and compiler-checked; what it cannot
//! check is *meaning*, and that residue is the price of the boundary.
//!
//! see rules: agent-mcp

mod args;
mod resources;
mod result;
mod verbs;

pub use args::*;
pub use resources::{ResourceError, Resources, SampleBuffer};
pub use result::{Diag, DocumentView, EditResult, OperatorInfo, Operators, PortInfo, Report};
pub use verbs::*;
