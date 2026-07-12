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
//! buffer), [`resolver`] (the fetch-on-miss in-memory
//! [`ResourceResolver`](reuben_core::resources::ResourceResolver)), [`decode`] (WAV bytes →
//! `SampleBuffer`, hound-in-WASM), and [`shell`] (the whole lifecycle state machine). Only
//! [`bridge`] — the `#[no_mangle]` shims and the `log` import — is `wasm32`-gated.
//! `cargo test` on the host exercises the real logic; the dedicated CI job also builds the
//! wasm artifact.
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

// The web-chat agent tool-schema artifact generator (issue #354, ADR-0054 §3). HOST-ONLY: it is
// consumed off-line by the proxy + the JS layer via the committed `js/tool-schemas.generated.json`,
// never called from the worklet, so it stays out of the wasm payload (issue #227 mobile budget).
//
// SAFE-REMOVAL GUARD (WX-3, issue #417): the Phase-3 extraction (WX-14) deletes this module from
// the public crate, and that deletion must not touch the wasm build. The `#[cfg]` below keeps it
// out of every wasm build; the `host_only_guard` tests keep that true by construction. See that
// module for the full argument — the reachers stay host-only (the `gen_tool_schemas` example + the
// staleness test), never the wasm-reachable surface.
#[cfg(not(target_arch = "wasm32"))]
pub mod tool_schema;

// The host-only safe-removal guard for [`tool_schema`] (WX-3, issue #417). Plain `cargo test`
// assertions — they run on the host, alongside CI's wasm build which is the ultimate backstop.
#[cfg(test)]
mod host_only_guard;

#[cfg(target_arch = "wasm32")]
mod bridge;
