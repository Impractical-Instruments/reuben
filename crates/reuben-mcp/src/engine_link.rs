//! The engine-facing link the five engine tools drive, and the socket under it.
//!
//! The window owns the channel — framing, timeouts, what a reply means. What is left here is this
//! door's transport: a fresh, bounded, blocking loopback TCP connection per exchange, retaining
//! nothing between calls, so the link survives the engine restarting under it.
//!
//! see rules: agent-mcp

use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use reuben_api::engine::{Channel, Transport, DEFAULT_STRUCTURE_ADDR};

/// How long to wait for the loopback connect before declaring the engine unreachable. Loopback
/// connects resolve in well under a millisecond when a server is up; this ceiling only bounds the
/// firewalled/unresponsive case so the probe never blocks the sidecar.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// This door's [`Transport`]: one short-lived loopback TCP connection per exchange.
#[derive(Debug, Clone)]
pub struct TcpTransport {
    addr: String,
    connect_timeout: Duration,
}

impl TcpTransport {
    /// A transport dialing `addr` (e.g. `127.0.0.1:9124`) with the given connect budget.
    pub fn new(addr: impl Into<String>, connect_timeout: Duration) -> Self {
        Self {
            addr: addr.into(),
            connect_timeout,
        }
    }
}

impl Transport for TcpTransport {
    fn round_trip(&self, line: &str, read_timeout: Duration) -> io::Result<String> {
        // Resolve to a concrete SocketAddr — connect_timeout needs one (and is what bounds the
        // connect; a plain `connect` could block far longer than our budget).
        let addr = self.addr.to_socket_addrs()?.next().ok_or_else(|| {
            io::Error::other(format!("no socket address resolved for {}", self.addr))
        })?;

        let stream = TcpStream::connect_timeout(&addr, self.connect_timeout)?;
        stream.set_read_timeout(Some(read_timeout))?;
        stream.set_write_timeout(Some(read_timeout))?;
        let _ = stream.set_nodelay(true);

        // Write the one request line. `&TcpStream: Write`, so no try_clone is needed to split the
        // socket — the reader below borrows the same stream.
        (&stream).write_all(line.as_bytes())?;
        (&stream).flush()?;

        // Read exactly one response line (one response per request).
        let mut reader = BufReader::new(&stream);
        let mut response = String::new();
        if reader.read_line(&mut response)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the structure channel closed before answering",
            ));
        }
        Ok(response)
    }

    fn endpoint(&self) -> &str {
        &self.addr
    }
}

/// The engine link: a handle to the one channel every engine tool speaks.
#[derive(Debug)]
pub struct EngineLink {
    structure: Channel,
}

impl EngineLink {
    /// A link dialing `structure_addr` over loopback TCP with this door's connect budget.
    pub fn new(structure_addr: impl Into<String>) -> Self {
        Self::from_channel(Channel::new(TcpTransport::new(
            structure_addr,
            DEFAULT_CONNECT_TIMEOUT,
        )))
    }

    /// A link over an already-built channel — the injection point for tests, which pair it with a
    /// fake transport.
    pub fn from_channel(structure: Channel) -> Self {
        Self { structure }
    }

    /// The structure channel — liveness, `send`, `swap`, `get_document`, `get_diagnostics`.
    pub fn structure(&self) -> &Channel {
        &self.structure
    }

    /// The structure-channel endpoint address, for `engine_status`.
    pub fn structure_endpoint(&self) -> String {
        self.structure.endpoint().to_string()
    }
}

impl Default for EngineLink {
    /// A link targeting the shared default address — the same one `reuben play` binds, so the
    /// sidecar and engine can never drift.
    fn default() -> Self {
        Self::new(DEFAULT_STRUCTURE_ADDR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_link_dials_exactly_what_reuben_play_binds() {
        // Asserted on the REAL construction path — the one `ReubenServer::new` builds.
        let link = EngineLink::default();
        assert_eq!(link.structure_endpoint(), DEFAULT_STRUCTURE_ADDR);
        assert!(
            link.structure_endpoint().starts_with("127.0.0.1:"),
            "the structure channel must stay loopback-only: {}",
            link.structure_endpoint()
        );
    }
}
