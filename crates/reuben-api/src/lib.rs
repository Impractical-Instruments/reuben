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
//! A browser worklet or a bare-metal embedder takes `default-features = false,
//! features = ["render"]`, which compiles neither the authoring surface nor `reuben-document`
//! beneath it, and links no `std`.
//!
//! Two seams face the other way — things a host must **provide** rather than call. The resource
//! resolver is the one call *in*: samples and nested documents are never handed to the engine as
//! bytes, so the engine calls back out through [`resources::Resources`], and [`fs_resolver`] is a
//! filesystem implementation to share rather than reimplement, behind a default-off feature. The
//! other is [`engine::EngineHost`]: the device map, the counters, the control ingress and the
//! clock the deferred free waits on — what only a host can know.

// Unconditional rather than `reuben-core`'s `cfg_attr(not(test), no_std)`: the exemption would
// spare three `alloc` imports here against ~380 there, and paying them leaves the lib test target
// `no_std` too. No `std` feature — nothing is additive and no command line can forget it.
#![no_std]

// Heap types are named through `alloc` in both halves. Never in the extern prelude, in any crate,
// so this line does not become removable later.
extern crate alloc;

// The authoring half needs an OS: `fs_resolver` calls `std::fs`, and the serde/schemars stack is
// `std`-shaped. With `render` alone this line is absent and the crate links no `std`.
//
// THE HOLE: a crate-root declaration resolves `std` from anywhere in the crate, the render half
// included, so a stray `use std::…` there survives every workspace-wide command. Only a
// render-only build catches it — CI runs one, and locally it is
// `cargo clippy -p reuben-api --no-default-features --features render`.
#[cfg(feature = "authoring")]
extern crate std;

#[cfg(feature = "authoring")]
pub mod authoring;

#[cfg(feature = "authoring")]
pub mod engine;

#[cfg(feature = "authoring")]
pub mod schema;

#[cfg(feature = "authoring")]
pub mod tools;

#[cfg(feature = "fs-resolver")]
pub mod fs_resolver;

#[cfg(feature = "render")]
pub mod render;

#[cfg(feature = "authoring")]
pub mod resources;

#[cfg(feature = "fs-resolver")]
pub use fs_resolver::FsResolver;
