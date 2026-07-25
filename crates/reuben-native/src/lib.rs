//! reuben-native — the removable native layer. see rules: execution-runtime
//!
//! Wraps the portable [`reuben_core`] embed surface (construct, `queue_osc`, `fill`/`fill_duplex`,
//! `drain_outbound`) with OS-specific I/O: [`osc`] decodes external OSC/UDP into
//! [`Message`](reuben_core::Message)s, [`audio`] drives a cpal output stream, [`input`] is the
//! cpal input ring feeding it, [`diagnostics`] is the shared xrun/ring counter surface,
//! [`structure`] is the loopback structure channel the MCP sidecar talks to, [`rigs`] holds the
//! default playable rig, and [`profile`] is the device profile (`--io-map`). Sample data resolves
//! through [`reuben_api::FsResolver`].

pub mod audio;
pub mod cli;
pub mod diagnostics;
pub mod input;
pub mod library;
pub mod osc;
pub mod profile;
pub mod rigs;
pub mod scaffold;
pub mod structure;
#[doc(hidden)]
pub mod test_support;

pub use reuben_core::Engine;

/// Re-export so embedders only depend on this crate.
pub use reuben_core;
