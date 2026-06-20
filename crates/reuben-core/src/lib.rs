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
pub mod descriptor;
pub mod graph;
pub mod message;
pub mod operator;
pub mod operators;
pub mod pitch;
pub mod plan;
pub mod render;
pub mod signal;
pub mod tuning;

pub use config::AudioConfig;
pub use descriptor::Descriptor;
pub use graph::{Graph, NodeKey};
pub use message::{Arg, Message};
pub use operator::{Io, Operator};
pub use plan::{Plan, PlanError};
pub use render::{Renderer, SerialExecutor};
