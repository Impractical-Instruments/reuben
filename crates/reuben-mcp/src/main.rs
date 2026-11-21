//! The reuben MCP shim binary, and the composition root: it builds the `current_thread` tokio
//! runtime, injects the real [`EngineLink`], and drives the stdio server to completion.

use std::env::VarError;

use reuben_api::engine::{Token, DEFAULT_STRUCTURE_ADDR};
use reuben_mcp::{EngineLink, STRUCTURE_ADDR_ENV, STRUCTURE_TOKEN_ENV};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let addr = env(STRUCTURE_ADDR_ENV).unwrap_or_else(|| DEFAULT_STRUCTURE_ADDR.to_string());
    let token = env(STRUCTURE_TOKEN_ENV).map(|token| {
        Token::new(token).unwrap_or_else(|empty| {
            eprintln!("reuben-mcp: {STRUCTURE_TOKEN_ENV}: {empty}");
            std::process::exit(2);
        })
    });
    let engine = EngineLink::new(addr, token);

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()?;

    runtime.block_on(reuben_mcp::serve_stdio(engine))
}

/// The variable's value; `None` when unset. Exits when it is set but not UTF-8, rather than
/// dialing the default as though it were unset.
fn env(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) => Some(value),
        Err(VarError::NotPresent) => None,
        Err(VarError::NotUnicode(_)) => {
            eprintln!("reuben-mcp: {name} is not valid UTF-8");
            std::process::exit(2);
        }
    }
}
