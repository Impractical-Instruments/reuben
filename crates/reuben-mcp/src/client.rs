//! The structure-channel client (owned by reuben-mcp): the
//! sidecar's half of the sidecar↔engine structure channel a live `reuben play` presents. It dials
//! the engine's loopback TCP structure channel and exchanges the shared
//! [`reuben_core::coordinator`] NDJSON envelope — one [`Request`] line out, one [`Response`] line
//! back, per the one-response-per-request framing — reusing the wire types **verbatim**
//! (no re-declaration).
//!
//! # Transport: `std::net` with timeouts, not `tokio::net`
//!
//! reuben-mcp is the workspace's only async member, but the tokio it carries is the
//! `current_thread` runtime fenced to `sync/rt/time/io-std` — **no `net` feature, no OS reactor**
//! (that feature set is measured sufficient; adding `net` pulls mio and a reactor for nothing).
//! So the channel is blocking [`std::net::TcpStream`] bounded by an explicit
//! [`connect_timeout`](TcpStream::connect_timeout) and read/write timeouts. Each exchange is one
//! short, bounded, blocking round trip — cheap enough to run directly, and crucially unable to
//! hang the sidecar: a dead port is refused at once, and a *wedged* server (accepts, never
//! answers) trips the read timeout instead of blocking forever.
//! see rules: agent-mcp
//!
//! # Fail fast
//!
//! The shim never spawns `reuben play`. A connect failure, a resolution failure, or a timeout is
//! a [`StructureError::Unreachable`] whose message carries the actionable
//! [`crate::ENGINE_UNREACHABLE_GUIDANCE`] ("start `reuben play`") — never a hang, never a panic.

use std::fmt;
use std::io::{self, BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use reuben_core::coordinator::{
    Conflict, ControlMessage, DiagnosticsReport, DocSource, DocumentSnapshot, Request, Response,
};
use reuben_core::SwapReport;

use crate::ENGINE_UNREACHABLE_GUIDANCE;

/// How long to wait for the loopback connect before declaring the engine unreachable. Loopback
/// connects resolve in well under a millisecond when a server is up; this ceiling only bounds the
/// firewalled/unresponsive case so the probe never blocks the sidecar.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);

/// How long to wait for the one response line before giving up on a *wedged* server (connected but
/// silent). Generous enough for a real swap's off-thread engine rebuild, tight enough that a hung
/// engine surfaces as a fail-fast rather than a stalled tool call.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The read budget for `ping` specifically. A pong is **immediate** — the structure server answers
/// `Ping` with `Pong` doing no work, unlike a `swap`'s off-thread engine rebuild — so
/// the liveness probe (`engine_status`) need not inherit the generous
/// [`DEFAULT_READ_TIMEOUT`]: a wedged engine surfaces as unreachable ~5× sooner. Still comfortably
/// above loopback + scheduler jitter, so a live-but-momentarily-busy engine is never misjudged dead.
const DEFAULT_PING_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// A failed structure-channel exchange. [`Unreachable`](Self::Unreachable) is the fail-fast case
/// — connect/timeout/I/O died — and its message names the fix; the other two are the
/// channel answering but unusably: [`Channel`](Self::Channel) is a server-sent [`Response::Error`]
/// (a channel-level fault, distinct from a domain answer that reports failure), and
/// [`Protocol`](Self::Protocol) is an unparseable or wrong-variant response.
#[derive(Debug)]
pub enum StructureError {
    /// The engine could not be reached (connect refused, address unresolved, or a read/write
    /// timeout on a wedged server). Carries the "start `reuben play`" guidance.
    Unreachable(String),
    /// The server framed a [`Response::Error`] — the request was understood as a channel message
    /// but produced no domain answer (e.g. an unreadable request, or a not-yet-wired verb).
    Channel(String),
    /// The response could not be parsed, or was a variant this verb never expects.
    Protocol(String),
}

impl StructureError {
    /// Build the fail-fast unreachable error, prefixing the shared actionable guidance so any
    /// caller that surfaces the message tells the user how to fix it.
    fn unreachable(cause: impl fmt::Display) -> Self {
        StructureError::Unreachable(format!("{ENGINE_UNREACHABLE_GUIDANCE} (cause: {cause})"))
    }

    /// Whether this is the unreachable-engine case — the branch the fail-fast guidance is for, and
    /// the branch an engine tool maps to the "start `reuben play`" result (act-then-map, #318).
    pub fn is_unreachable(&self) -> bool {
        matches!(self, StructureError::Unreachable(_))
    }
}

impl fmt::Display for StructureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StructureError::Unreachable(m) => write!(f, "{m}"),
            StructureError::Channel(m) => write!(f, "structure channel error: {m}"),
            StructureError::Protocol(m) => write!(f, "structure channel protocol error: {m}"),
        }
    }
}

impl std::error::Error for StructureError {}

/// The outcome of a `swap` that reached the engine (transport failures are [`StructureError`]).
/// Both are legitimate answers the tool surface (#318) maps as it chooses: an install report
/// (which itself may carry `ok: false` load errors — the channel *worked*), or an
/// `expect`-guard conflict the client reconciles by re-reading. Both arms carry the wire's own
/// type, so nothing is re-declared on the way up.
#[derive(Debug, Clone, PartialEq)]
pub enum SwapOutcome {
    /// The engine processed the swap and returned its [`SwapReport`] (success or load-failure).
    Installed(SwapReport),
    /// The `expect` guard missed: nothing installed; the [`Conflict`] names what keeps playing.
    Conflict(Conflict),
}

/// The one thing a structure channel must be able to do: hand a request line to the engine and
/// return the response line. **The injectable seam** — the shipping [`TcpTransport`] is the real
/// loopback socket; a test double returns canned NDJSON (or an [`io::Error`]) so the tool bodies
/// above still exercise real serialization, real parsing, and the real
/// [`Unreachable`](StructureError::Unreachable) mapping.
///
/// Deliberately the *lowest* seam. Above this line live the things a fake should exercise rather
/// than replace: NDJSON framing and parsing plus the unreachable/protocol split
/// ([`StructureClient::exchange_with`]), and the wrong-variant classification each verb does. Below
/// it live the socket mechanics — connect, the `set_*_timeout` calls, read-to-newline — which
/// `tests/structure_client.rs` still drives over real TCP.
pub trait StructureTransport: Send + Sync + fmt::Debug {
    /// One request line out, one response line back. `read_timeout` is per-call because `ping`
    /// runs on a tighter budget than the other verbs. Any I/O failure — refused connect,
    /// unresolved address, timeout, or a peer that closed before answering — is an
    /// [`io::Error`]; the caller classifies it.
    fn round_trip(&self, line: &str, read_timeout: Duration) -> io::Result<String>;

    /// The endpoint this transport dials, for `engine_status` and diagnostics.
    fn endpoint(&self) -> &str;
}

/// The shipping [`StructureTransport`]: a fresh, bounded, blocking loopback TCP connection per
/// exchange. Nothing is retained between calls, so the link survives the engine restarting under
/// it, and a dead port is refused at once rather than hanging the sidecar.
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

impl StructureTransport for TcpTransport {
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

        // Read exactly one response line (one response per request). A read timeout on
        // a wedged server surfaces here as an Err, not a hang.
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

/// A client for one engine's loopback structure channel: the NDJSON framing, the response-variant
/// classification, and the timeout policy, over an injectable [`StructureTransport`].
#[derive(Debug)]
pub struct StructureClient {
    transport: Box<dyn StructureTransport>,
    read_timeout: Duration,
    /// The tighter read budget `ping` uses (its pong is immediate) — never longer than the general
    /// `read_timeout`, and capped at [`DEFAULT_PING_READ_TIMEOUT`]. See [`Self::ping`].
    ping_read_timeout: Duration,
}

impl StructureClient {
    /// A client dialing `addr` (e.g. `127.0.0.1:9124`) over real TCP with the default timeouts.
    pub fn new(addr: impl Into<String>) -> Self {
        Self::with_timeouts(addr, DEFAULT_CONNECT_TIMEOUT, DEFAULT_READ_TIMEOUT)
    }

    /// A client with explicit connect/read timeouts — the seam the wedged-server test drives to
    /// prove a silent engine trips the read timeout fast instead of hanging.
    pub fn with_timeouts(
        addr: impl Into<String>,
        connect_timeout: Duration,
        read_timeout: Duration,
    ) -> Self {
        Self::with_transport(TcpTransport::new(addr, connect_timeout), read_timeout)
    }

    /// A client over an explicit transport — the injection point for tests. `read_timeout` is the
    /// general per-verb budget; `ping`'s tighter budget is derived from it.
    pub fn with_transport(
        transport: impl StructureTransport + 'static,
        read_timeout: Duration,
    ) -> Self {
        Self {
            transport: Box::new(transport),
            read_timeout,
            // The pong is immediate, so `ping` uses the tighter of the two budgets: never longer
            // than a caller's explicit read timeout (a deliberately tiny one still wins — the
            // wedged-server test relies on that), but capped at the ping default when it's generous.
            ping_read_timeout: read_timeout.min(DEFAULT_PING_READ_TIMEOUT),
        }
    }

    /// The address this client dials.
    pub fn addr(&self) -> &str {
        self.transport.endpoint()
    }

    /// Liveness: `Ok(())` iff the channel answered [`Response::Pong`]. This is what
    /// [`EngineLink`](crate::EngineLink) consults for engine reachability (`engine_status`). Every
    /// other engine tool acts-then-maps its own exchange instead of probing first.
    pub fn ping(&self) -> Result<(), StructureError> {
        // The pong is immediate, so bound this exchange by the tighter `ping_read_timeout` rather
        // than the general read budget a swap earns — a wedged engine fails fast.
        match self.exchange_with(&Request::Ping, self.ping_read_timeout)? {
            Response::Pong => Ok(()),
            other => Err(unexpected("ping", "pong", &other)),
        }
    }

    /// Install a document, accepted **by value or by path** — both branches are
    /// exposed here; which the tool surface offers is #318's call. An optional
    /// `expect` content-hash guard rejects the swap on mismatch.
    pub fn swap(
        &self,
        source: DocSource,
        expect: Option<String>,
    ) -> Result<SwapOutcome, StructureError> {
        match self.exchange(&Request::Swap { source, expect })? {
            Response::SwapReport(report) => Ok(SwapOutcome::Installed(report)),
            Response::Conflict(conflict) => Ok(SwapOutcome::Conflict(conflict)),
            Response::Error { message } => Err(StructureError::Channel(message)),
            other => Err(unexpected("swap", "swap_report/conflict", &other)),
        }
    }

    /// Read the canonical installed document and its content hash: a fresh
    /// conversation attaches to what's playing in one call.
    pub fn get_document(&self) -> Result<DocumentSnapshot, StructureError> {
        match self.exchange(&Request::GetDocument)? {
            Response::Document(snapshot) => Ok(snapshot),
            Response::Error { message } => Err(StructureError::Channel(message)),
            other => Err(unexpected("get_document", "document", &other)),
        }
    }

    /// Audition a batch of control values on the running engine.
    ///
    /// One exchange carries the whole batch and the engine queues it as one unit, so the gesture
    /// cannot half-apply and no concurrent client interleaves into the middle of it. The engine
    /// converges this with external OSC at its own ingress; a message whose address routes nowhere
    /// is dropped there, so a successful return means "received and queued", not "applied".
    ///
    /// An empty or over-long batch is refused by the engine as a
    /// [`Channel`](StructureError::Channel) error rather than acked.
    pub fn send(&self, messages: Vec<ControlMessage>) -> Result<(), StructureError> {
        match self.exchange(&Request::Send { messages })? {
            Response::Sent => Ok(()),
            Response::Error { message } => Err(StructureError::Channel(message)),
            other => Err(unexpected("send", "sent", &other)),
        }
    }

    /// Read the engine's running diagnostics counters.
    pub fn get_diagnostics(&self) -> Result<DiagnosticsReport, StructureError> {
        match self.exchange(&Request::GetDiagnostics)? {
            Response::Diagnostics(report) => Ok(report),
            Response::Error { message } => Err(StructureError::Channel(message)),
            other => Err(unexpected("get_diagnostics", "diagnostics", &other)),
        }
    }

    /// One request → one response over a fresh connection (the NDJSON framing), bounded by the
    /// general [`read_timeout`](Self::read_timeout) — the budget every verb but `ping` uses (a real
    /// swap's off-thread rebuild earns it). `ping` calls [`exchange_with`](Self::exchange_with)
    /// directly with its tighter budget.
    fn exchange(&self, request: &Request) -> Result<Response, StructureError> {
        self.exchange_with(request, self.read_timeout)
    }

    /// [`exchange`](Self::exchange), but with an explicit read/write budget for this one call — so
    /// `ping` can fail fast on its immediate pong without loosening (or tightening) the general
    /// budget the other verbs share.
    ///
    /// Everything policy-shaped lives here, above the socket: NDJSON out, one line back, and the
    /// two-way classification — any transport [`io::Error`] is the fail-fast
    /// [`StructureError::Unreachable`] carrying the "start `reuben play`" guidance, while a line
    /// that came back but will not parse is a [`StructureError::Protocol`]. A fake transport
    /// therefore exercises this whole path for real.
    fn exchange_with(
        &self,
        request: &Request,
        read_timeout: Duration,
    ) -> Result<Response, StructureError> {
        let line = self
            .transport
            .round_trip(&request.to_ndjson(), read_timeout)
            .map_err(StructureError::unreachable)?;
        Response::from_ndjson(&line).map_err(|e| StructureError::Protocol(e.to_string()))
    }
}

/// The wrong-response-variant protocol error, spelled once so every verb reports it the same way.
fn unexpected(verb: &str, want: &str, got: &Response) -> StructureError {
    StructureError::Protocol(format!("{verb} expected {want}, got {got:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_error_carries_the_start_reuben_play_guidance() {
        // The fail-fast contract: the unreachable message names the fix, whatever
        // the underlying cause, so a caller that surfaces it is actionable.
        let err = StructureError::unreachable("connection refused");
        assert!(err.is_unreachable());
        let shown = err.to_string();
        assert!(
            shown.contains("reuben play"),
            "unreachable must name the fix: {shown}"
        );
        assert!(
            shown.contains("connection refused"),
            "unreachable should preserve the cause for debugging: {shown}"
        );
    }

    #[test]
    fn channel_and_protocol_errors_are_not_unreachable() {
        // A server that answers (even with Response::Error) is not the fail-fast unreachable case —
        // the engine is up; the request just didn't produce a domain answer.
        assert!(!StructureError::Channel("unreadable request".to_string()).is_unreachable());
        assert!(!StructureError::Protocol("bad json".to_string()).is_unreachable());
    }
}
