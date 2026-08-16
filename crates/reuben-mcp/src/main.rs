//! The reuben MCP shim binary, and the composition root: it builds the `current_thread` tokio
//! runtime, injects the real [`EngineLink`], and drives the stdio server to completion.

use reuben_mcp::EngineLink;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    runtime.block_on(reuben_mcp::serve_stdio(EngineLink::default()))
}
