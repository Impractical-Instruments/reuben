//! The structure-channel client (ADR-0046 §8, owned by reuben-mcp per ADR-0044 §3): the
//! sidecar's half of the sidecar↔engine control channel a live `reuben play` presents. It dials
//! the engine's loopback TCP structure channel and exchanges the shared
//! [`reuben_core::coordinator`] NDJSON envelope — one [`Request`] line out, one [`Response`] line
//! back, per ADR-0046 §8's one-response-per-request framing — reusing the wire types **verbatim**
//! (no re-declaration).
//!
//! # Transport: `std::net` with timeouts, not `tokio::net`
//!
//! reuben-mcp is the workspace's only async member (ADR-0044 §3), but the tokio it carries is the
//! `current_thread` runtime fenced to `sync/rt/time/io-std` — **no `net` feature, no OS reactor**
//! (ADR-0044 §5 measured that as sufficient; adding `net` pulls mio and a reactor for nothing).
//! So the channel is blocking [`std::net::TcpStream`] bounded by an explicit
//! [`connect_timeout`](TcpStream::connect_timeout) and read/write timeouts. Each exchange is one
//! short, bounded, blocking round trip — cheap enough to run directly, and crucially unable to
//! hang the sidecar: a dead port is refused at once, and a *wedged* server (accepts, never
//! answers) trips the read timeout instead of blocking forever.
//!
//! # Fail fast (ADR-0044 §2)
//!
//! The shim never spawns `reuben play`. A connect failure, a resolution failure, or a timeout is
//! a [`StructureError::Unreachable`] whose message carries the actionable
//! [`crate::ENGINE_UNREACHABLE_GUIDANCE`] ("start `reuben play`") — never a hang, never a panic.

use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use reuben_core::coordinator::{
    DiagnosticsReport, DocSource, Request, Response, DEFAULT_STRUCTURE_ADDR,
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
/// `Ping` with `Pong` doing no work (ADR-0046 §8), unlike a `swap`'s off-thread engine rebuild — so
/// the liveness probe (`engine_status`, and the probe-first `send`) need not inherit the generous
/// [`DEFAULT_READ_TIMEOUT`]: a wedged engine surfaces as unreachable ~5× sooner. Still comfortably
/// above loopback + scheduler jitter, so a live-but-momentarily-busy engine is never misjudged dead.
const DEFAULT_PING_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// A failed structure-channel exchange. [`Unreachable`](Self::Unreachable) is the fail-fast case
/// (ADR-0044 §2) — connect/timeout/I/O died — and its message names the fix; the other two are the
/// channel answering but unusably: [`Channel`](Self::Channel) is a server-sent [`Response::Error`]
/// (a channel-level fault, distinct from a domain answer that reports failure — ADR-0048 §3), and
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

/// The engine's answer to `get_document` (ADR-0046 §8): the canonical installed document paired
/// with its [`content_hash`](reuben_core::content_hash) — the token a later swap's `expect` guard
/// compares (ADR-0046 §9). Reads the raw [`Response::Document`] fields without re-validating: the
/// engine is the single validation authority (ADR-0045 §3).
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentSnapshot {
    /// The installed document as raw JSON — exactly what the engine is playing.
    pub document: serde_json::Value,
    /// The installed document's content hash.
    pub content_hash: String,
}

/// The outcome of a `swap` that reached the engine (transport failures are [`StructureError`]).
/// Both are legitimate answers the tool surface (#318) maps as it chooses: an install report
/// (which itself may carry `ok: false` load errors — the channel *worked*, ADR-0048 §3), or an
/// `expect`-guard conflict the client reconciles by re-reading (ADR-0046 §9).
#[derive(Debug, Clone, PartialEq)]
pub enum SwapOutcome {
    /// The engine processed the swap and returned its [`SwapReport`] (success or load-failure).
    Installed(SwapReport),
    /// The `expect` guard missed: nothing installed; `actual` is the hash still playing.
    Conflict {
        /// The hash the client asserted was installed.
        expected: String,
        /// The hash actually still playing — re-read via `get_document` to reconcile.
        actual: String,
    },
}

/// A client for one engine's loopback structure channel. Cheap to hold and clone (an address plus
/// two timeouts); it opens a fresh short-lived connection per exchange, so nothing is retained
/// between calls and a client survives the engine restarting under it.
#[derive(Debug, Clone)]
pub struct StructureClient {
    addr: String,
    connect_timeout: Duration,
    read_timeout: Duration,
    /// The tighter read budget `ping` uses (its pong is immediate) — never longer than the general
    /// `read_timeout`, and capped at [`DEFAULT_PING_READ_TIMEOUT`]. See [`Self::ping`].
    ping_read_timeout: Duration,
}

impl StructureClient {
    /// A client dialing `addr` (e.g. `127.0.0.1:9124`) with the default timeouts.
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
        Self {
            addr: addr.into(),
            connect_timeout,
            read_timeout,
            // The pong is immediate, so `ping` uses the tighter of the two budgets: never longer
            // than a caller's explicit read timeout (a deliberately tiny one still wins — the
            // wedged-server test relies on that), but capped at the ping default when it's generous.
            ping_read_timeout: read_timeout.min(DEFAULT_PING_READ_TIMEOUT),
        }
    }

    /// The address this client dials.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Liveness (ADR-0046 §8): `Ok(())` iff the channel answered [`Response::Pong`]. This is what
    /// [`EngineLink`](crate::EngineLink) consults for engine reachability (`engine_status`, and the
    /// probe-first `send`).
    pub fn ping(&self) -> Result<(), StructureError> {
        // The pong is immediate, so bound this exchange by the tighter `ping_read_timeout` rather
        // than the general read budget a swap earns — a wedged engine fails fast (ADR-0044 §2).
        match self.exchange_with(&Request::Ping, self.ping_read_timeout)? {
            Response::Pong => Ok(()),
            other => Err(unexpected("ping", "pong", &other)),
        }
    }

    /// Install a document (ADR-0046 §8), accepted **by value or by path** — both branches are
    /// exposed here; which the tool surface offers is #318's call (ADR-0048 §2). An optional
    /// `expect` content-hash guard rejects the swap on mismatch (ADR-0046 §9).
    pub fn swap(
        &self,
        source: DocSource,
        expect: Option<String>,
    ) -> Result<SwapOutcome, StructureError> {
        match self.exchange(&Request::Swap { source, expect })? {
            Response::SwapReport(report) => Ok(SwapOutcome::Installed(report)),
            Response::Conflict { expected, actual } => {
                Ok(SwapOutcome::Conflict { expected, actual })
            }
            Response::Error { message } => Err(StructureError::Channel(message)),
            other => Err(unexpected("swap", "swap_report/conflict", &other)),
        }
    }

    /// Read the canonical installed document and its content hash (ADR-0046 §8): a fresh
    /// conversation attaches to what's playing in one call.
    pub fn get_document(&self) -> Result<DocumentSnapshot, StructureError> {
        match self.exchange(&Request::GetDocument)? {
            Response::Document {
                document,
                content_hash,
            } => Ok(DocumentSnapshot {
                document,
                content_hash,
            }),
            Response::Error { message } => Err(StructureError::Channel(message)),
            other => Err(unexpected("get_document", "document", &other)),
        }
    }

    /// Read the engine's running diagnostics counters (ADR-0046 §8 / ADR-0048 §6).
    pub fn get_diagnostics(&self) -> Result<DiagnosticsReport, StructureError> {
        match self.exchange(&Request::GetDiagnostics)? {
            Response::Diagnostics(report) => Ok(report),
            Response::Error { message } => Err(StructureError::Channel(message)),
            other => Err(unexpected("get_diagnostics", "diagnostics", &other)),
        }
    }

    /// One request → one response over a fresh connection (ADR-0046 §8's framing), bounded by the
    /// general [`read_timeout`](Self::read_timeout) — the budget every verb but `ping` uses (a real
    /// swap's off-thread rebuild earns it). `ping` calls [`exchange_with`](Self::exchange_with)
    /// directly with its tighter budget.
    fn exchange(&self, request: &Request) -> Result<Response, StructureError> {
        self.exchange_with(request, self.read_timeout)
    }

    /// [`exchange`](Self::exchange), but with an explicit read/write budget for this one call — so
    /// `ping` can fail fast on its immediate pong without loosening (or tightening) the general
    /// budget the other verbs share. Connect is always bounded by [`connect_timeout`](Self::connect_timeout);
    /// any I/O or timeout failure is the fail-fast [`StructureError::Unreachable`] (ADR-0044 §2).
    fn exchange_with(
        &self,
        request: &Request,
        read_timeout: Duration,
    ) -> Result<Response, StructureError> {
        // Resolve to a concrete SocketAddr — connect_timeout needs one (and is what bounds the
        // connect; a plain `connect` could block far longer than our budget).
        let addr = self
            .addr
            .to_socket_addrs()
            .map_err(StructureError::unreachable)?
            .next()
            .ok_or_else(|| {
                StructureError::unreachable(format!("no socket address resolved for {}", self.addr))
            })?;

        let stream = TcpStream::connect_timeout(&addr, self.connect_timeout)
            .map_err(StructureError::unreachable)?;
        stream
            .set_read_timeout(Some(read_timeout))
            .map_err(StructureError::unreachable)?;
        stream
            .set_write_timeout(Some(read_timeout))
            .map_err(StructureError::unreachable)?;
        let _ = stream.set_nodelay(true);

        // Write the one request line. `&TcpStream: Write`, so no try_clone is needed to split the
        // socket — the reader below borrows the same stream.
        (&stream)
            .write_all(request.to_ndjson().as_bytes())
            .map_err(StructureError::unreachable)?;
        (&stream).flush().map_err(StructureError::unreachable)?;

        // Read exactly one response line (ADR-0046 §8: one response per request). A read timeout on
        // a wedged server surfaces here as an Err → Unreachable, not a hang.
        let mut reader = BufReader::new(&stream);
        let mut line = String::new();
        let read = reader
            .read_line(&mut line)
            .map_err(StructureError::unreachable)?;
        if read == 0 {
            return Err(StructureError::unreachable(
                "the structure channel closed before answering",
            ));
        }
        Response::from_ndjson(&line).map_err(|e| StructureError::Protocol(e.to_string()))
    }
}

impl Default for StructureClient {
    /// A client targeting the shared [`DEFAULT_STRUCTURE_ADDR`] — the same address `reuben play`
    /// binds, so server and client can never drift.
    fn default() -> Self {
        Self::new(DEFAULT_STRUCTURE_ADDR)
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
        // The fail-fast contract (ADR-0044 §2): the unreachable message names the fix, whatever
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

    #[test]
    fn default_client_targets_the_shared_addr() {
        // Server and client share one address const (no drift): the default client dials exactly
        // what `reuben play` binds.
        assert_eq!(StructureClient::default().addr(), DEFAULT_STRUCTURE_ADDR);
    }
}
