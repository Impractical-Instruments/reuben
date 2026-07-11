//! The reuben MCP shim binary (ADR-0044 §1): the MCP client spawns it per conversation and closes
//! stdin to shut it down. It builds the `current_thread` tokio runtime ADR-0044 §5 measured as
//! sufficient — `rt`/`time`/`io-std`, no OS reactor (`enable_io`/`net` would pull mio for nothing;
//! stdio rides tokio's blocking pool) — and drives the stdio server to completion.
//!
//! This is the composition root: it injects the real engine-liveness probe — a [`PingProbe`]
//! dialing the shared `reuben_core::coordinator::DEFAULT_STRUCTURE_ADDR` over the structure channel
//! (#315) — so the engine tools fail fast only when a live `reuben play` is genuinely unreachable
//! (ADR-0044 §2). The structure channel is blocking `std::net`, so it needs no tokio `net`/reactor
//! feature; the fence stays intact.

use reuben_mcp::PingProbe;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    runtime.block_on(reuben_mcp::serve_stdio(Box::new(PingProbe::default())))
}
