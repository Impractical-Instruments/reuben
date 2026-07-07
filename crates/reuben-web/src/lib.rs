//! reuben-web — the WebAudio shell over the shared embed surface (issue #224, P2 of the
//! web player epic #151).
//!
//! One of three shells around [`reuben_core::engine`] (ADR-0039): native wraps it with
//! cpal + UDP/OSC + fs; **this crate** wraps it for the browser — a `wasm32-unknown-unknown`
//! cdylib driven by an `AudioWorkletProcessor`, one `Engine::fill` per 128-frame worklet
//! quantum; the game shell (#222) is the third. The boundary is **raw C-ABI, no
//! `wasm-bindgen`** (ADR-0040): flat exports + one `log` import, because bindgen's glue
//! fights `AudioWorkletGlobalScope` (P1 finding, #223).
//!
//! Layout: everything testable is plain host Rust — [`codec`] (the flat tagged control
//! buffer), [`resolver`] (the fetch-on-miss in-memory [`ResourceResolver`]
//! (reuben_core::resources::ResourceResolver)), [`decode`] (WAV bytes → `SampleBuffer`,
//! hound-in-WASM), and [`shell`] (the whole lifecycle state machine). Only [`bridge`] — the
//! `#[no_mangle]` shims and the `log` import — is `wasm32`-gated. `cargo test` on the host
//! exercises the real logic; the dedicated CI job also builds the wasm artifact.
//!
//! The crate is deliberately **detached** from the repo workspace (own `[workspace]` table)
//! so root `cargo test/clippy/fmt --workspace` never touch it; see `Cargo.toml`.
//!
//! The ES-module JS API (worklet processor, main-thread fetch-on-miss loader, control
//! encoder) is co-located under `js/` and codes against [`bridge`]'s documented ABI.

pub mod codec;
pub mod decode;
pub mod resolver;
pub mod shell;

#[cfg(target_arch = "wasm32")]
mod bridge;
