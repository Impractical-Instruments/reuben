//! reuben-core — the portable, OS-free realtime core (ADR-0012).
//!
//! Data model: [`signal`] (audio-rate) and [`message`] (discrete, OSC-shaped). Authoring:
//! [`operator`] + [`descriptor`]. Composition: [`graph`] → [`plan`] (Instantiate) →
//! [`render`] (per-block execution). Musical layer: [`pitch`] + [`tuning`]. The MVP
//! operator set is in [`operators`].
//!
//! This crate has no OS dependencies; audio I/O and protocol adapters live in the
//! removable native layer.

pub mod config;
pub mod context;
pub mod descriptor;
pub mod format;
pub mod graph;
pub mod message;
pub mod operator;
pub mod operators;
pub mod pitch;
pub mod plan;
pub mod registry;
pub mod render;
pub mod resources;
pub mod schema;
pub mod signal;
pub mod tuning;

pub use config::AudioConfig;
pub use context::{Chord, ChordTag, Context, ScaleField, SnapDir, SnapPolicy, SnapTarget};
pub use descriptor::Descriptor;
pub use format::{load, load_instrument, InstrumentDoc, LoadError, LoadWarning, Loaded};
pub use graph::{Graph, NodeKey};
pub use message::{Arg, Message};
pub use operator::{Io, Operator};
pub use plan::{Plan, PlanError};
pub use registry::Registry;
pub use render::{Renderer, SerialExecutor};
pub use resources::{
    ResolveError, ResolvedRefs, ResourceResolver, ResourceStore, SampleBuffer, SampleId,
};
