//! reuben-native — the removable native layer (ADR-0012).
//!
//! This crate is the seam where OS-specific I/O lives. It wraps the portable
//! [`reuben_core`] embed surface ([`reuben_core::engine`] — construct, `queue_osc`,
//! `fill`/`fill_duplex`, `drain_outbound`) with:
//! - [`osc`] — decoding external OSC/UDP packets into core [`Message`](reuben_core::Message)s.
//! - [`audio`] — a cpal output stream driving the engine live.
//! - [`input`] — the cpal input stream (P5/#182, ADR-0038 §8/§9): a lock-free SPSC ring from
//!   the input callback into the output callback, resampled + drift-compensated into the
//!   engine rate, with the device→logical input channel map.
//! - [`diagnostics`] — the shared xrun/ring counter surface (ADR-0038 §9) and its periodic
//!   stderr logging; [`audio`] feeds it output-deadline misses, [`input`] feeds it input-ring
//!   underruns, overruns, and producer-backstop drops.
//! - [`structure`] — the loopback-TCP/NDJSON structure channel (ADR-0046 §8): a std thread in
//!   `reuben play` answering `ping`/`get_document`/`get_diagnostics`/`swap` (the last a
//!   stop-the-world restart, ADR-0046 §10) for the MCP sidecar.
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
pub mod input;
pub mod library;
pub mod osc;
pub mod profile;
pub mod resources;
pub mod rigs;
pub mod scaffold;
pub mod structure;

pub use reuben_core::Engine;

/// Re-export so embedders only depend on this crate.
pub use reuben_core;
