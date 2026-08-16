//! reuben-native — the removable native layer.
//!
//! Wraps the window's render surface ([`reuben_api::render`]: the Coordinator + RenderSlot pair,
//! `queue_osc`, `fill`/`fill_duplex`, `drain_outbound`) with OS-specific I/O: [`osc`] decodes
//! external OSC/UDP into flat control args, [`audio`] drives a cpal output stream, [`input`] is the
//! cpal input ring feeding it, [`diagnostics`] is the shared xrun/ring counter surface,
//! [`structure`] is the loopback structure channel the MCP sidecar talks to, [`rigs`] holds the
//! default playable rig, and [`profile`] is the device profile (`--io-map`). Sample data resolves
//! through [`reuben_api::FsResolver`].
//!
//! It reaches the engine through [`reuben_api`] and nowhere else: this crate is a door, and a door
//! that could also reach past the window would be a second surface to keep honest.

pub mod audio;
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
