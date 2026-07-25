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
//! resource seam are the engine's, re-exported. [`resources`] states that split and why.
//!
//! see rules: agent-mcp

mod args;
mod resources;
mod result;
mod verbs;

pub use args::*;
pub use resources::{ResolveError, Resources, SampleBuffer};
pub use result::{Diag, DocumentView, EditResult, OperatorInfo, Operators, PortInfo, Report};
pub use verbs::*;
