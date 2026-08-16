//! The client side of the structure channel: NDJSON framing out, one response line back,
//! classified into the verb's answer.
//!
//! Two invariants a caller may rely on: every exchange is bounded (the door's [`Transport`] holds
//! the socket and the budget it is handed), and every transport failure becomes a
//! [`ChannelError::Unreachable`] carrying the window's start-the-engine guidance — so this module
//! never hangs and never panics.
//!
//! What is *not* here is the socket. A door supplies the [`Transport`]: loopback TCP for the MCP
//! sidecar, whatever an in-process host has instead. The framing, the timeout policy and the
//! response classification are the window's, so two doors cannot disagree about what a reply means.

use std::fmt;
use std::io;
use std::time::Duration;

use super::prose::ENGINE_UNREACHABLE_GUIDANCE;
use super::wire::{
    Conflict, ControlMessage, DiagnosticsReport, DocSource, DocumentSnapshot, Request, Response,
    SwapReport,
};

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

/// A failed structure-channel exchange.
#[derive(Debug)]
pub enum ChannelError {
    /// The engine could not be reached (connect refused, address unresolved, or a read/write
    /// timeout on a wedged server). Carries the "start `reuben play`" guidance.
    Unreachable(String),
    /// The server framed a [`Response::Error`] — the request was understood as a channel message
    /// but produced no domain answer (e.g. an unreadable request, or a not-yet-wired verb).
    Channel(String),
    /// The response could not be parsed, or was a variant this verb never expects.
    Protocol(String),
}

impl ChannelError {
    /// Build the unreachable error, prefixing the shared guidance so any caller that surfaces the
    /// message is actionable. Keeps the cause for debugging.
    fn unreachable(cause: impl fmt::Display) -> Self {
        ChannelError::Unreachable(format!("{ENGINE_UNREACHABLE_GUIDANCE} (cause: {cause})"))
    }

    /// Whether this is the unreachable-engine case.
    pub fn is_unreachable(&self) -> bool {
        matches!(self, ChannelError::Unreachable(_))
    }
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelError::Unreachable(m) => write!(f, "{m}"),
            ChannelError::Channel(m) => write!(f, "structure channel error: {m}"),
            ChannelError::Protocol(m) => write!(f, "structure channel protocol error: {m}"),
        }
    }
}

impl std::error::Error for ChannelError {}

/// The outcome of a `swap` that reached the engine — both arms are answers, not failures
/// (transport failures are [`ChannelError`]).
#[derive(Debug, Clone, PartialEq)]
pub enum SwapOutcome {
    /// The engine processed the swap and returned its [`SwapReport`] (success or load-failure).
    Installed(SwapReport),
    /// The `expect` guard missed: nothing installed; the [`Conflict`] names what keeps playing.
    Conflict(Conflict),
}

/// The one thing a structure channel must be able to do: hand a request line to the engine and
/// return the response line.
///
/// **The injectable seam**, and deliberately the lowest one: everything above it — framing,
/// parsing, the unreachable/protocol split — is exercised rather than replaced by a test double.
/// Below it live the socket mechanics, which are the door's — a loopback TCP one ships with the
/// MCP sidecar and is driven over a real socket there.
pub trait Transport: Send + Sync + fmt::Debug {
    /// One request line out, one response line back. `read_timeout` is per-call because `ping`
    /// runs on a tighter budget than the other verbs. Any I/O failure — refused connect,
    /// unresolved address, timeout, or a peer that closed before answering — is an
    /// [`io::Error`]; the caller classifies it.
    fn round_trip(&self, line: &str, read_timeout: Duration) -> io::Result<String>;

    /// The endpoint this transport dials, for `engine_status` and diagnostics.
    fn endpoint(&self) -> &str;
}

/// One engine's structure channel: the NDJSON framing, the response-variant classification, and
/// the timeout policy, over the door's [`Transport`].
#[derive(Debug)]
pub struct Channel {
    transport: Box<dyn Transport>,
    read_timeout: Duration,
    /// The tighter read budget `ping` uses (its pong is immediate) — never longer than the general
    /// `read_timeout`, and capped at [`DEFAULT_PING_READ_TIMEOUT`]. See [`Self::ping`].
    ping_read_timeout: Duration,
}

impl Channel {
    /// A channel over the door's transport, on the default per-verb read budget.
    pub fn new(transport: impl Transport + 'static) -> Self {
        Self::with_read_timeout(transport, DEFAULT_READ_TIMEOUT)
    }

    /// A channel with an explicit general read budget; `ping`'s tighter one is derived from it.
    pub fn with_read_timeout(transport: impl Transport + 'static, read_timeout: Duration) -> Self {
        Self {
            transport: Box::new(transport),
            read_timeout,
            // The pong is immediate, so `ping` uses the tighter of the two budgets: never longer
            // than a caller's explicit read timeout (a deliberately tiny one still wins — the
            // wedged-server test relies on that), but capped at the ping default when it's generous.
            ping_read_timeout: read_timeout.min(DEFAULT_PING_READ_TIMEOUT),
        }
    }

    /// The endpoint this channel reaches.
    pub fn endpoint(&self) -> &str {
        self.transport.endpoint()
    }

    /// Liveness: `Ok(())` iff the channel answered [`Response::Pong`]. The only probe on the
    /// channel — every other verb acts and maps its own failure.
    pub fn ping(&self) -> Result<(), ChannelError> {
        match self.exchange_with(&Request::Ping, self.ping_read_timeout)? {
            Response::Pong => Ok(()),
            other => Err(unexpected("ping", "pong", &other)),
        }
    }

    /// Install a document, by value or by path. An optional `expect` content-hash guard rejects
    /// the swap on mismatch.
    pub fn swap(
        &self,
        source: DocSource,
        expect: Option<String>,
    ) -> Result<SwapOutcome, ChannelError> {
        match self.exchange(&Request::Swap { source, expect })? {
            Response::SwapReport(report) => Ok(SwapOutcome::Installed(report)),
            Response::Conflict(conflict) => Ok(SwapOutcome::Conflict(conflict)),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("swap", "swap_report/conflict", &other)),
        }
    }

    /// Read the canonical installed document and its content hash.
    pub fn get_document(&self) -> Result<DocumentSnapshot, ChannelError> {
        match self.exchange(&Request::GetDocument)? {
            Response::Document(snapshot) => Ok(snapshot),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("get_document", "document", &other)),
        }
    }

    /// Audition a batch of control values on the running engine.
    ///
    /// `Ok` means "received and queued", NOT "applied": a message whose address routes nowhere is
    /// dropped at the engine's ingress. An empty or over-long batch is refused by the engine as a
    /// [`Channel`](ChannelError::Channel) error rather than acked.
    pub fn send(&self, messages: Vec<ControlMessage>) -> Result<(), ChannelError> {
        match self.exchange(&Request::Send { messages })? {
            Response::Sent => Ok(()),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("send", "sent", &other)),
        }
    }

    /// Read the engine's running diagnostics counters.
    pub fn get_diagnostics(&self) -> Result<DiagnosticsReport, ChannelError> {
        match self.exchange(&Request::GetDiagnostics)? {
            Response::Diagnostics(report) => Ok(report),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("get_diagnostics", "diagnostics", &other)),
        }
    }

    /// One request → one response on the general budget, which every verb but `ping` uses.
    fn exchange(&self, request: &Request) -> Result<Response, ChannelError> {
        self.exchange_with(request, self.read_timeout)
    }

    /// [`exchange`](Self::exchange) with an explicit per-call read/write budget.
    ///
    /// The policy layer, above the socket: framing out, one line back, and the split between a
    /// transport failure (unreachable) and a line that came back unparseable (protocol).
    fn exchange_with(
        &self,
        request: &Request,
        read_timeout: Duration,
    ) -> Result<Response, ChannelError> {
        let line = self
            .transport
            .round_trip(&request.to_ndjson(), read_timeout)
            .map_err(ChannelError::unreachable)?;
        Response::from_ndjson(&line).map_err(|e| ChannelError::Protocol(e.to_string()))
    }
}

/// The wrong-response-variant protocol error, spelled once so every verb reports it the same way.
fn unexpected(verb: &str, want: &str, got: &Response) -> ChannelError {
    ChannelError::Protocol(format!("{verb} expected {want}, got {got:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreachable_error_carries_the_start_reuben_play_guidance() {
        let err = ChannelError::unreachable("connection refused");
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
        assert!(!ChannelError::Channel("unreadable request".to_string()).is_unreachable());
        assert!(!ChannelError::Protocol("bad json".to_string()).is_unreachable());
    }
}
