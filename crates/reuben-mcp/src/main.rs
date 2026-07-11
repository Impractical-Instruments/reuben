//! The reuben MCP shim binary (ADR-0044 §1): the MCP client spawns it per conversation and closes
//! stdin to shut it down. It builds the `current_thread` tokio runtime ADR-0044 §5 measured as
//! sufficient — `rt`/`time`/`io-std`, no OS reactor (`enable_io`/`net` would pull mio for nothing;
//! stdio rides tokio's blocking pool) — and drives the stdio server to completion.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    runtime.block_on(reuben_mcp::serve_stdio())
}
