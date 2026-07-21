//! The reuben MCP shim binary: the MCP client spawns it per conversation and closes
//! stdin to shut it down. It builds the minimal `current_thread` tokio runtime —
//! `rt`/`time`/`io-std`, no OS reactor (`enable_io`/`net` would pull mio for nothing;
//! stdio rides tokio's blocking pool) — and drives the stdio server to completion.
//!
//! This is the composition root: it injects the real engine channel — an [`EngineLink`] dialing
//! the shared `reuben_core::coordinator::DEFAULT_STRUCTURE_ADDR` over the structure channel (#315)
//! and `reuben_mcp::default_osc_addr` for OSC control — so the engine tools reach a live
//! `reuben play` and fail fast only when it is genuinely unreachable. The structure
//! channel is blocking `std::net`, so it needs no tokio `net`/reactor feature; the fence stays intact.
//!
//! see rules: agent-mcp

use reuben_mcp::EngineLink;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    runtime.block_on(reuben_mcp::serve_stdio(EngineLink::default()))
}
