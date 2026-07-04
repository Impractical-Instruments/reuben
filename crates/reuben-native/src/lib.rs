//! reuben-native — the removable native layer (ADR-0012).
//!
//! This crate is the seam where OS-specific I/O lives. It wraps the portable
//! [`reuben_core`] engine with:
//! - [`engine`] — an [`Engine`] that owns a Plan + Renderer and fills arbitrary-length
//!   output buffers from a queue of incoming Messages (the bridge between the fixed
//!   block-size core and a real-time audio callback).
//! - [`osc`] — decoding external OSC/UDP packets into core [`Message`](reuben_core::Message)s.
//! - [`audio`] — a cpal output stream driving the engine live.
//! - [`diagnostics`] — the shared xrun/ring counter surface (ADR-0038 §9) and its periodic
//!   stderr logging; [`audio`] feeds it output-deadline misses, P5 will feed it input-ring
//!   under/overruns.
//! - [`resources`] — a filesystem + WAV [`ResourceResolver`](reuben_core::resources::ResourceResolver)
//!   decoding sample data for the sample player (ADR-0016).
//! - [`rigs`] — ready-made instrument graphs (the default playable rig for now).
//! - [`profile`] — the device profile (`--io-map`, ADR-0038 §6/§7): logical↔device channel
//!   maps, device selection, and sample-rate/buffer-size preferences, outside the patch.
//!
//! The portable core does all the DSP; everything here is I/O glue and is meant to be
//! swappable per platform (or removed entirely when embedding the core elsewhere).

pub mod audio;
pub mod cli;
pub mod diagnostics;
pub mod engine;
pub mod osc;
pub mod profile;
pub mod resources;
pub mod rigs;
pub mod scaffold;

pub use engine::Engine;

/// Re-export so embedders only depend on this crate.
pub use reuben_core;
