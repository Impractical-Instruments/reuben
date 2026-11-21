//! The loopback TCP [`Transport`]: what a process outside the engine dials `reuben play`'s
//! structure channel with — the MCP sidecar, and the editor attached to an engine it did not start.
//!
//! A fresh, bounded, blocking connection per exchange, retaining nothing between calls, so a
//! client survives the engine restarting under it. A subscription is the one connection kept open:
//! [`TcpTransport::subscribe`] dials its own and reads events from it until either end closes.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use std::io::{self, BufReader, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::channel::{unanswered, Transport};
use super::token::{credential_line, Token};
use super::wire::{read_ndjson_line, Event, Request, Response, MAX_LINE_BYTES};

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
    /// Sent as each connection's first line, ahead of its request.
    token: Option<Token>,
}

impl TcpTransport {
    /// A transport dialing `addr` (e.g. `127.0.0.1:9124`) with the given connect budget.
    pub fn new(addr: impl Into<String>, connect_timeout: Duration) -> Self {
        Self {
            addr: addr.into(),
            resolved: None,
            connect_timeout,
            token: None,
        }
    }

    /// This transport, opening every connection with `token`'s credential line. A server on
    /// another machine serves nothing without it; a loopback one needs none and accepts it.
    pub fn with_token(self, token: Option<Token>) -> Self {
        Self { token, ..self }
    }

    /// `line` behind the credential line, when this transport has a token.
    fn framed(&self, line: &str) -> String {
        match &self.token {
            Some(token) => credential_line(token) + line,
            None => line.into(),
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
            token: None,
        })
    }

    /// Subscribe to the engine's changes on a connection of its own: one [`Event`] per change, in
    /// the order the engine made them, for as long as the connection stays open.
    ///
    /// The connect and the engine's acknowledgement are bounded by the connect budget; after that
    /// a read waits as long as the next change takes. The iterator ends when the engine closes the
    /// connection, and after the first read error.
    pub fn subscribe(&self) -> io::Result<impl Iterator<Item = io::Result<Event>>> {
        let stream = self.connect()?;
        stream.set_read_timeout(Some(self.connect_timeout))?;
        stream.set_write_timeout(Some(self.connect_timeout))?;
        (&stream).write_all(self.framed(&Request::Subscribe.to_ndjson()).as_bytes())?;
        (&stream).flush()?;

        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        if read_ndjson_line(&mut reader, &mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the structure channel closed before answering",
            ));
        }
        match Response::from_ndjson(&line) {
            Ok(Response::Subscribed) => {}
            Ok(Response::Error { message }) => return Err(io::Error::other(message)),
            Ok(other) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("expected a subscription, the engine answered {other:?}"),
                ))
            }
            Err(e) => return Err(io::Error::new(io::ErrorKind::InvalidData, e)),
        }
        reader.get_ref().set_read_timeout(None)?;
        Ok(Events {
            reader: Some(reader),
            line: String::new(),
        })
    }

    /// Dial the engine within the connect budget.
    fn connect(&self) -> io::Result<TcpStream> {
        // `connect_timeout` takes one resolved address, and is what bounds the connect: a plain
        // `connect` could block far longer than the budget. Each address a name resolves to is
        // tried in turn, as `localhost` may resolve to `::1` before the `127.0.0.1` a server binds.
        let mut last = None;
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
                    let _ = stream.set_nodelay(true);
                    return Ok(stream);
                }
                Err(e) => last = Some(e),
            }
        }
        Err(last.unwrap_or_else(|| {
            io::Error::other(format!("no socket address resolved for {}", self.addr))
        }))
    }
}

/// The events of one subscription, read a line at a time. `reader` is `None` once the stream has
/// ended or failed: a failed read may have left it mid-line, so nothing after it is trusted.
struct Events {
    reader: Option<BufReader<TcpStream>>,
    line: String,
}

impl Iterator for Events {
    type Item = io::Result<Event>;

    fn next(&mut self) -> Option<Self::Item> {
        let reader = self.reader.as_mut()?;
        self.line.clear();
        match read_ndjson_line(reader, &mut self.line) {
            Ok(0) => {
                self.reader = None;
                None
            }
            Ok(_) => Some(
                Event::from_ndjson(&self.line)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e)),
            ),
            Err(e) => {
                self.reader = None;
                Some(Err(e))
            }
        }
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
        let stream = self.connect()?;
        stream.set_read_timeout(Some(read_timeout))?;
        stream.set_write_timeout(Some(read_timeout))?;

        // Write the one request line. `&TcpStream: Write`, so no try_clone is needed to split the
        // socket — the reader below borrows the same stream.
        (&stream).write_all(self.framed(line).as_bytes())?;
        (&stream).flush()?;

        // Read exactly one response line (one response per request), no longer than the cap. The
        // request is the engine's now: a read that runs out of budget is a missing answer, not a
        // missing engine.
        let mut reader = BufReader::new(&stream);
        let mut response = String::new();
        let read = read_ndjson_line(&mut reader, &mut response).map_err(|e| match e.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => unanswered(e),
            _ => e,
        })?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the structure channel closed before answering",
            ));
        }
        Ok(response)
    }

    fn subscribe(&self) -> io::Result<Box<dyn Iterator<Item = io::Result<Event>> + Send>> {
        Ok(Box::new(TcpTransport::subscribe(self)?))
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
    fn a_read_timeout_after_the_request_was_written_is_marked_unanswered() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let (done, wait) = std::sync::mpsc::channel::<()>();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut line = String::new();
            std::io::BufRead::read_line(&mut BufReader::new(&stream), &mut line).expect("read");
            // Hold the connection open, answering nothing, until the client has given up.
            let _ = wait.recv();
        });
        let transport = TcpTransport::new(&addr, DEFAULT_CONNECT_TIMEOUT);
        let err = transport
            .round_trip("{\"ping\":1}\n", Duration::from_millis(200))
            .expect_err("no answer");
        drop(done);
        server.join().expect("server");
        assert!(
            err.get_ref()
                .is_some_and(|cause| cause.is::<super::super::channel::Unanswered>()),
            "{err:?}"
        );
    }

    #[test]
    fn a_refused_connect_is_not_marked_unanswered() {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .expect("a free port")
            .port();
        let transport = TcpTransport::new(format!("127.0.0.1:{port}"), DEFAULT_CONNECT_TIMEOUT);
        let err = transport
            .round_trip("{\"ping\":1}\n", Duration::from_millis(200))
            .expect_err("nothing listens");
        assert!(
            !err.get_ref()
                .is_some_and(|cause| cause.is::<super::super::channel::Unanswered>()),
            "{err:?}"
        );
    }

    #[test]
    fn a_transport_with_a_token_sends_it_first() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr").to_string();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("accept");
            let mut reader = BufReader::new(&stream);
            let (mut first, mut second) = (String::new(), String::new());
            std::io::BufRead::read_line(&mut reader, &mut first).expect("first");
            std::io::BufRead::read_line(&mut reader, &mut second).expect("second");
            (&stream)
                .write_all(Response::Pong.to_ndjson().as_bytes())
                .expect("answer");
            (first, second)
        });
        let token = Token::new("s3cret").expect("a token");
        let transport =
            TcpTransport::new(&addr, DEFAULT_CONNECT_TIMEOUT).with_token(Some(token.clone()));
        let reply = transport
            .round_trip(&Request::Ping.to_ndjson(), Duration::from_secs(1))
            .expect("answered");
        assert_eq!(
            Response::from_ndjson(&reply).expect("a reply"),
            Response::Pong
        );
        let (first, second) = server.join().expect("server");
        assert_eq!(first, credential_line(&token));
        assert_eq!(second, Request::Ping.to_ndjson());
        assert!(
            !format!("{transport:?}").contains("s3cret"),
            "a transport's Debug must not print its token"
        );
    }

    #[test]
    fn a_name_that_does_not_resolve_is_refused_up_front() {
        assert!(TcpTransport::resolved("not an address", DEFAULT_CONNECT_TIMEOUT).is_err());
    }
}
