//! reuben-native — the removable native layer.
//!
//! Wraps the window's render surface ([`reuben_api::render`]: the Coordinator + RenderSlot pair,
//! `queue_osc`, `fill`/`fill_duplex`, `drain_outbound`) with OS-specific I/O: [`osc`] decodes
//! external OSC/UDP into flat control args, [`control`] carries them to the render callback on
//! bounded ring pairs, [`audio`] drives a cpal output stream, [`input`] is the
//! cpal input ring feeding it, [`devices`] lists the output devices a profile can name,
//! [`diagnostics`] is the shared xrun/ring counter surface, [`meters`] the levels at the device,
//! [`engine_channel`] is the loopback engine channel the MCP sidecar talks to, [`rigs`] holds the
//! default playable rig, [`live`] is the text-editor client (`reuben send`, `reuben live`), and
//! [`profile`] is the device profile (`--io-map`). Sample data resolves
//! through [`reuben_api::FsResolver`].
//!
//! [`engine::start`] composes all of them into a running engine with one call — `reuben play` is a
//! thin caller of it, and any other binary embedding the engine starts it the same way, from the
//! same flags ([`cli`]).
//!
//! It reaches the engine through [`reuben_api`] and nowhere else: this crate is a door, and a door
//! that could also reach past the window would be a second surface to keep honest.

pub mod audio;
pub mod cli;
pub mod client;
pub mod control;
pub mod devices;
pub mod diagnostics;
pub mod engine;
pub mod engine_channel;
pub mod input;
pub mod library;
pub mod live;
pub mod meters;
pub mod midi;
pub mod osc;
mod probes;
pub mod profile;
pub mod rigs;
pub mod scaffold;
#[doc(hidden)]
pub mod test_support;
pub mod tree_watch;
