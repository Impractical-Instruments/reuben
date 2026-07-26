//! reuben-api — the one window between [`reuben_core`] and every consumer.
//!
//! Every host goes through here: the MCP sidecar, the native CLI, the browser worklet. The window
//! decides what is exposed rather than mirroring what exists behind it, so the crates under it stay
//! free to pick representations that suit the engine rather than the wire.
//!
//! Two halves with opposite cost profiles, each behind its own feature so a host compiles only the
//! one it drives:
//!
//! - the **authoring/control** half — called from anywhere, off-thread, where the budget is
//!   irrelevant and every type is serialized to a consumer. [`authoring`] is document work,
//!   [`engine`] is reaching a *running* engine (swap, control, status, diagnostics) from either end
//!   of the structure channel, and [`tools`] is the roster the two are advertised through.
//! - [`render`] — called from the audio callback, where a conversion per block *is* the cost.
//!
//! A browser worklet takes `default-features = false, features = ["render"]` and never compiles the
//! authoring surface.
//!
//! Two seams face the other way — things a host must **provide** rather than call. The resource
//! resolver is the one call *in*: samples and nested documents are never handed to the engine as
//! bytes, so the engine calls back out through [`authoring::Resources`], and [`fs_resolver`] is a
//! filesystem implementation to share rather than reimplement, behind a default-off feature. The
//! other is [`engine::EngineHost`]: the device map, the counters, the control ingress and the
//! clock the deferred free waits on — what only a host can know.
//!
//! see rules: agent-mcp

#[cfg(feature = "authoring")]
pub mod authoring;

#[cfg(feature = "authoring")]
pub mod engine;

#[cfg(feature = "authoring")]
pub mod tools;

#[cfg(feature = "fs-resolver")]
pub mod fs_resolver;

#[cfg(feature = "render")]
pub mod render;

#[cfg(feature = "fs-resolver")]
pub use fs_resolver::FsResolver;
