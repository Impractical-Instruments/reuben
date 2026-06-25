//! reuben-core — the portable, OS-free realtime core (ADR-0012).
//!
//! Data model: [`signal`] (audio-rate) and [`message`] (discrete, OSC-shaped). Authoring:
//! [`operator`] + [`descriptor`]. Composition: [`graph`] → [`plan`] (Instantiate) →
//! [`render`] (per-block execution). Musical layer: [`vocab`] (`pitch`/`harmony`) + [`tuning`]. The MVP
//! operator set is in [`operators`].
//!
//! This crate has no OS dependencies; audio I/O and protocol adapters live in the
//! removable native layer.

// The `operator_contract!` macro (ADR-0025) expands to fully-qualified `::reuben_core::…` paths so
// it works for any embedder. Inside this crate, that name must resolve to *us* — hence the alias.
extern crate self as reuben_core;

/// Crate-private `Io`-construction bridge for the per-operator micro benchmarks (#30, ADR-0019).
/// Gated behind the non-default `bench` feature so the bridge never leaks into the public API:
/// the `[[bench]]` micro targets declare `required-features = ["bench"]`, and CI runs them (plus
/// the forcing-function test below) with `--features bench`. A normal build/publish never compiles
/// it, so the `pub(crate)` `Io` builders it reaches for stay internal.
#[cfg(feature = "bench")]
pub mod bench_support;

pub mod boundary;
pub mod config;
pub mod descriptor;
pub mod format;
pub mod graph;
pub mod message;
pub mod operator;
pub mod operators;
pub mod plan;
pub mod registry;
pub mod render;
pub mod resources;
pub mod schema;
pub mod signal;
pub mod tuning;
pub mod vocab;

pub use config::AudioConfig;
pub use descriptor::Descriptor;
pub use format::{load, load_instrument, InstrumentDoc, LoadError, LoadWarning, Loaded};
pub use graph::{Graph, NodeKey};
pub use message::{Arg, Message};
pub use operator::{Io, Operator};
pub use plan::{Plan, PlanError};
pub use vocab::{Chord, ChordTag, Harmony, ScaleField, SnapDir, SnapPolicy, SnapTarget};
// The single-source operator contract macro (ADR-0025). Re-exported at the crate root so operator
// modules can call `crate::operator_contract!(..)`, mirroring `register_operator!`.
pub use registry::Registry;
pub use reuben_macros::operator_contract;
// `#[derive(ArgValue)]` (ADR-0030): integrates a shared `vocab` type with the central `Arg`.
pub use reuben_macros::ArgValue;
// Re-export the self-registration macro at the crate root so operator modules can call
// `crate::register_operator!(..)` regardless of module declaration order (ADR-0024).
pub(crate) use registry::register_operator;
pub use render::{Renderer, SerialExecutor};
pub use resources::{
    ResolveError, ResolvedRefs, ResourceResolver, ResourceStore, SampleBuffer, SampleId,
};
