//! reuben-core — the portable, OS-free realtime core.
//!
//! Data model: [`signal`] (audio-rate) and [`message`] (discrete, OSC-shaped). Operator surface:
//! [`operator`] + [`descriptor`]. Composition: [`graph`] → [`plan`] (Instantiate) →
//! [`render`] (per-block execution). Musical layer: [`vocab`] (`pitch`/`harmony`) + [`tuning`]. The MVP
//! operator set is in [`operators`].
//!
//! This crate is the render half. It has no OS dependencies and names no crate above it — authoring
//! an instrument document and turning it into a [`Plan`] is `reuben-document`'s job, and audio I/O
//! and protocol adapters live in the removable native layer.

// The `operator_contract!` macro expands to fully-qualified `::reuben_core::…` paths so
// it works for any embedder. Inside this crate, that name must resolve to *us* — hence the alias.
extern crate self as reuben_core;

/// Crate-private `Io`-construction bridge for the per-operator micro benchmarks.
/// Gated behind the non-default `bench` feature so the bridge never leaks into the public API:
/// the `[[bench]]` micro targets declare `required-features = ["bench"]`, and CI runs them (plus
/// the forcing-function test below) with `--features bench`. A normal build/publish never compiles
/// it, so the `pub(crate)` `Io` builders it reaches for stay internal.
#[cfg(feature = "bench")]
pub mod bench_support;

/// In-crate harness that drives a single operator through the real engine (`Plan` + `Renderer`) for
/// unit tests and benches — the one implementation of "descriptor → wired `Io`", so a test can't
/// drift from production seeding/stepping. Gated to test/bench builds: it reaches `Renderer`'s
/// `pub(crate)` `step_node` seam, kept out of the public render API.
#[cfg(any(test, feature = "bench"))]
pub mod op_driver;

pub mod boundary;
pub mod config;
pub mod coordinator;
pub mod descriptor;
pub mod dsp;
pub mod engine;
pub mod graph;
pub mod message;
pub mod operator;
pub mod operators;
pub mod plan;
pub mod registry;
pub mod render;
pub mod resources;
pub mod signal;
pub mod tuning;
pub mod vocab;
pub mod wavetable;

pub use config::AudioConfig;
pub use descriptor::Descriptor;
pub use engine::Engine;
pub use graph::{Graph, Interface, NodeKey};
pub use message::{Arg, Message};
pub use operator::{Io, Operator};
pub use plan::{Plan, PlanError};
pub use vocab::{Chord, ChordTag, Harmony, ScaleField, SnapDir, SnapPolicy, SnapTarget};
pub use wavetable::Wavetable;
// The single-source operator contract macro. Re-exported at the crate root so operator
// modules can call `crate::operator_contract!(..)`, mirroring `register_operator!`.
pub use registry::Registry;
pub use reuben_macros::operator_contract;
// The pointwise-number-operator family macro: one scalar fn -> value+signal variants
// across number types. Re-exported here so operator modules call `crate::number_operator_contract!`.
pub use reuben_macros::number_operator_contract;
// The product-vocab unpack macro: one census line -> an `unpack_<type>` operator that
// destructures a product vocab type into its held fields. Re-exported so `operators/unpack.rs`
// calls `crate::unpack_op!`.
pub use reuben_macros::unpack_op;
// `#[derive(ArgValue)]`: integrates a shared `vocab` type with the central `Arg`.
pub use reuben_macros::ArgValue;
// Re-export the self-registration macro at the crate root so operator modules can call
// `crate::register_operator!(..)` regardless of module declaration order.
pub(crate) use registry::register_operator;
// Its boundary sibling: `crate::register_osc_form!(..)` submits a struct vocab type's external
// OSC form from its definition site, the same pattern.
pub(crate) use boundary::register_osc_form;
pub use render::{render_plan, RenderScratch, Renderer, SerialExecutor};
pub use resources::{ResolvedRefs, ResourceStore, SampleBuffer, SampleId};
// The audio-rate data vocabulary lives in `signal` (the single naming site for the element type +
// its owned/borrowed buffer forms). Adopted across the render spine; a raw `f32` buffer elsewhere
// is caught by scripts/check_sample_alias.py.
pub use signal::{AudioSample, BlockMut, BlockView};
