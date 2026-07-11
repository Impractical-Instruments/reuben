//! CLI introspection surface — a thin re-export of [`reuben_core::introspect`] (ADR-0044 §3).
//!
//! The pure describe/validate functions descended into core so one implementation serves
//! every door: this CLI, the MCP sidecar, and the web player. The paths here
//! (`reuben_native::cli::{describe, describe_patch, validate}` and the view types) stay
//! stable, so the `reuben` binary and embedders are unchanged.

pub use reuben_core::introspect::*;
