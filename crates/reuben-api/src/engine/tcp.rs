//! The loopback TCP [`Transport`]: what a process outside the engine dials `reuben play`'s
//! structure channel with — the MCP sidecar, and the editor attached to an engine it did not start.
//!
//! A fresh, bounded, blocking connection per exchange, retaining nothing between calls, so a
//! client survives the engine restarting under it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use std::io::{self, BufReader, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::channel::Transport;
use super::wire::{read_ndjson_line, MAX_LINE_BYTES};

/// How long to wait for the loopback connect before declaring the engine unreachable. Loopback
/// connects resolve in well under a millisecond when a server is up; this ceiling only bounds the
/// firewalled/unresponsive case so a probe never blocks its caller.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// A [`Transport`] over loopback TCP: one short-lived connection per exchange.
#[derive(Debug, Clone)]
pub struct TcpTransport {
    addr: String,
    /// What `addr` resolved to once, up front; `None` resolves it afresh on every exchange.
    resolved: Option<Vec<SocketAddr>>,
    connect_timeout: Duration,
}

impl TcpTransport {
    /// A transport dialing `addr` (e.g. `127.0.0.1:9124`) with the given connect budget.
    pub fn new(addr: impl Into<String>, connect_timeout: Duration) -> Self {
        Self {
            addr: addr.into(),
            resolved: None,
            connect_timeout,
        }
    }

    /// A transport dialing what `addr` resolves to now, for every exchange after: a name lookup
    /// that blocks is paid here rather than on each exchange. `Err` when it does not resolve.
    pub fn resolved(addr: impl Into<String>, connect_timeout: Duration) -> io::Result<Self> {
        let addr = addr.into();
        let resolved = addr.to_socket_addrs()?.collect();
        Ok(Self {
            addr,
            resolved: Some(resolved),
            connect_timeout,
        })
    }
}

impl Transport for TcpTransport {
    fn round_trip(&self, line: &str, read_timeout: Duration) -> io::Result<String> {
        // The server would refuse it, but only once it has read the cap; and it closes with the
        // rest unread, which resets the connection before a client still writing reads the reply.
        if line.len() > MAX_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("a structure channel request is longer than {MAX_LINE_BYTES} bytes"),
            ));
        }
        // `connect_timeout` takes one resolved address, and is what bounds the connect: a plain
        // `connect` could block far longer than the budget. Each address a name resolves to is
        // tried in turn, as `localhost` may resolve to `::1` before the `127.0.0.1` a server binds.
        let mut last = None;
        let mut connected = None;
        let looked_up;
        let addrs = match &self.resolved {
            Some(resolved) => resolved,
            None => {
                looked_up = self.addr.to_socket_addrs()?.collect::<Vec<_>>();
                &looked_up
            }
        };
        for addr in addrs {
            match TcpStream::connect_timeout(addr, self.connect_timeout) {
                Ok(stream) => {
                    connected = Some(stream);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let stream = match (connected, last) {
            (Some(stream), _) => stream,
            (None, Some(e)) => return Err(e),
            (None, None) => {
                return Err(io::Error::other(format!(
                    "no socket address resolved for {}",
                    self.addr
                )))
            }
        };
        stream.set_read_timeout(Some(read_timeout))?;
        stream.set_write_timeout(Some(read_timeout))?;
        let _ = stream.set_nodelay(true);

        // Write the one request line. `&TcpStream: Write`, so no try_clone is needed to split the
        // socket — the reader below borrows the same stream.
        (&stream).write_all(line.as_bytes())?;
        (&stream).flush()?;

        // Read exactly one response line (one response per request), no longer than the cap.
        let mut reader = BufReader::new(&stream);
        let mut response = String::new();
        if read_ndjson_line(&mut reader, &mut response)? == 0 {
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

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;

    #[test]
    fn a_request_past_the_cap_is_refused_before_dialing() {
        // Nothing listens on port 1, so a dial would fail as a refused connect instead.
        let transport = TcpTransport::new("127.0.0.1:1", DEFAULT_CONNECT_TIMEOUT);
        let line = "x".repeat(MAX_LINE_BYTES + 1);
        let err = transport
            .round_trip(&line, Duration::from_secs(1))
            .expect_err("over the cap");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData, "{err}");
    }

    #[test]
    fn a_transport_resolved_up_front_dials_what_the_name_resolved_to() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            std::io::BufRead::read_line(&mut BufReader::new(&stream), &mut line).expect("read");
            (&stream).write_all(line.as_bytes()).expect("echo");
        });
        let transport = TcpTransport::resolved(&addr, DEFAULT_CONNECT_TIMEOUT).expect("resolves");
        let reply = transport
            .round_trip("{\"ping\":1}\n", Duration::from_secs(1))
            .expect("answered");
        assert_eq!(reply.trim_end(), "{\"ping\":1}");
        assert_eq!(transport.endpoint(), addr);
        server.join().expect("server");
    }

    #[test]
    fn a_name_that_does_not_resolve_is_refused_up_front() {
        assert!(TcpTransport::resolved("not an address", DEFAULT_CONNECT_TIMEOUT).is_err());
    }
}
