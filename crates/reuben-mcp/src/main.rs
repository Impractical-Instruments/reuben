//! The reuben MCP shim binary, and the composition root: it builds the `current_thread` tokio
//! runtime, injects the real [`EngineLink`], and drives the stdio server to completion.

use std::env::VarError;

use reuben_mcp::{EngineLink, STRUCTURE_ADDR_ENV};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = match std::env::var(STRUCTURE_ADDR_ENV) {
        Ok(addr) => EngineLink::new(addr).unwrap_or_else(|refused| {
            eprintln!("reuben-mcp: {STRUCTURE_ADDR_ENV}: {refused}");
            std::process::exit(2);
        }),
        Err(VarError::NotPresent) => EngineLink::default(),
        Err(VarError::NotUnicode(_)) => {
            eprintln!("reuben-mcp: {STRUCTURE_ADDR_ENV} is not valid UTF-8");
            std::process::exit(2);
        }
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    runtime.block_on(reuben_mcp::serve_stdio(engine))
}
