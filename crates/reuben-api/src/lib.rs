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
//! A browser worklet — or a bare-metal embedder — takes `default-features = false,
//! features = ["render"]` and never compiles the authoring surface, nor `reuben-document` beneath
//! it. That build reaches a target with no filesystem, no serde and no `std` at all, which is what
//! the attribute below is for.
//!
//! Two seams face the other way — things a host must **provide** rather than call. The resource
//! resolver is the one call *in*: samples and nested documents are never handed to the engine as
//! bytes, so the engine calls back out through [`resources::Resources`], and [`fs_resolver`] is a
//! filesystem implementation to share rather than reimplement, behind a default-off feature. The
//! other is [`engine::EngineHost`]: the device map, the counters, the control ingress and the
//! clock the deferred free waits on — what only a host can know.

// Unconditional, unlike `reuben-core`'s `cfg_attr(not(test), no_std)`, and the difference is
// arithmetic rather than principle: the exemption there spares ~380 `alloc` imports in its
// `#[cfg(test)] mod tests`, where the same exemption here would spare three. Paying those three
// buys a strictly stronger attribute — the lib TEST target is `no_std` too, so no `std` prelude
// item can reach production code through a build shape that only tests compile. There is no `std`
// FEATURE, for `reuben-core`'s reason: nothing is additive and no command line can forget it.
#![no_std]

// The heap types are named through `alloc` in both halves, so an import reads the same whichever
// one compiles it. Never in the extern prelude — that is true of any crate, not just a `no_std`
// one, so this line does not become removable later.
extern crate alloc;

// The authoring half genuinely needs an OS: `fs_resolver` calls `std::fs`, and the serde/schemars
// stack under the document verbs is `std`-shaped throughout. Declared rather than assumed, because
// `#![no_std]` takes `std` out of the extern prelude — with the `render` feature alone, this line
// is absent and the crate links no `std` at all, which is the whole point of the split.
//
// KNOW THE HOLE IT LEAVES: it is a crate-root declaration, so while it is compiled `std` resolves
// from anywhere in the crate, the render half included. A stray `use std::…` in `render` therefore
// builds fine under the default features — every workspace-wide command compiles both halves — and
// is caught only by a render-only build. CI runs one on the host (the `check` job's render-only
// fence) and one on the cross target; locally it is
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
