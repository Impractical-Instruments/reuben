//! reuben-api — the one window between [`reuben_core`] and every consumer.
//!
//! Every host goes through here: the MCP sidecar, the native CLI, the browser worklet. The window
//! decides what is exposed rather than mirroring what exists behind it, so the crates under it stay
//! free to pick representations that suit the engine rather than the wire.
//!
//! Two halves with opposite cost profiles, each behind its own feature so a host compiles only the
//! one it drives:
//!
//! - [`authoring`] — called from anywhere, off-thread, where the budget is irrelevant and every
//!   type is serialized to a consumer.
//! - [`render`] — called from the audio callback, where a conversion per block *is* the cost.
//!
//! A browser worklet takes `default-features = false, features = ["render"]` and never compiles the
//! authoring surface.
//!
//! The resource resolver is the one call *in*: samples and nested documents are never handed to the
//! engine as bytes, so the engine calls back out through a seam the host provides.
//! [`fs_resolver`] is a filesystem implementation to share rather than reimplement — behind a
//! default-off feature, because a resolver that arrives switched on is one a host inherits instead
//! of choosing.
//!
//! see rules: agent-mcp

#[cfg(feature = "authoring")]
pub mod authoring;

#[cfg(feature = "fs-resolver")]
pub mod fs_resolver;

#[cfg(feature = "render")]
pub mod render;

#[cfg(feature = "fs-resolver")]
pub use fs_resolver::FsResolver;
