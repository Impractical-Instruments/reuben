//! The client side of the engine channel: NDJSON framing out, one response line back,
//! classified into the verb's answer.
//!
//! Two invariants a caller may rely on: every exchange is bounded (the door's [`Transport`] holds
//! the socket and the budget it is handed), and every transport failure becomes a
//! [`ChannelError`] — [`Unreachable`](ChannelError::Unreachable), carrying the window's
//! start-the-engine guidance, unless the request reached the engine and only its answer is
//! missing ([`NoAnswer`](ChannelError::NoAnswer)) — so this module never hangs and never panics.
//!
//! What is *not* here is the socket. A door supplies the [`Transport`]: TCP for a process
//! dialing `reuben play` (the `tcp` feature's), whatever an in-process host has instead. The
//! framing, the timeout policy and the response classification are the window's, so two doors
//! cannot disagree about what a reply means.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use std::fmt;
use std::io;
use std::time::Duration;

use super::prose::ENGINE_UNREACHABLE_GUIDANCE;
use super::wire::{
    over_long_probe_set_refusal, Conflict, ControlArg, ControlMessage, DiagnosticsReport,
    DocumentSnapshot, Event, HeldSwap, Library, ListWhat, MetersReport, Request, Response,
    SendRefusal, SwapReport, MAX_PROBES,
};

/// How long to wait for the one response line before giving up on a *wedged* server (connected but
/// silent), for a request that only reads or queues. Tight enough that a hung engine surfaces as a
/// fail-fast rather than a stalled tool call.
const DEFAULT_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The read budget for a request that may load and plan a whole instrument tree: `open` and `call` (an edit re-plans and installs the open instrument). Each decodes every sample the
/// tree references, and may first wait for the Coordinator behind another such request, so a
/// large sample set on a cold disk takes several times [`DEFAULT_READ_TIMEOUT`]. Past this the
/// answer is [`ChannelError::NoAnswer`], not a dead engine: the request may still be applied.
const DEFAULT_WORK_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// The read budget for `ping` specifically. A pong is **immediate** — the engine-channel server answers
/// `Ping` with `Pong` doing no work, unlike an `open`'s off-thread engine rebuild — so
/// the liveness probe (`engine_status`) need not inherit the generous
/// [`DEFAULT_READ_TIMEOUT`]: a wedged engine surfaces as unreachable ~5× sooner. Still comfortably
/// above loopback + scheduler jitter, so a live-but-momentarily-busy engine is never misjudged dead.
const DEFAULT_PING_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// A failed engine-channel exchange.
#[derive(Debug)]
pub enum ChannelError {
    /// The engine could not be reached (connect refused, address unresolved, a write timeout, or a
    /// liveness `ping` that went unanswered). Carries the "start `reuben play`" guidance.
    Unreachable(String),
    /// The request was written but no answer came within the read budget: the engine may still
    /// apply it. Carries the guidance to re-read before retrying.
    NoAnswer(String),
    /// The request produced no domain answer: the server framed a [`Response::Error`] (e.g. an
    /// unreadable request, or a not-yet-wired verb), or the client refused it before sending
    /// (e.g. a probe set over [`MAX_PROBES`]).
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

    /// The no-answer error for a request whose answer did not come within `budget`.
    fn no_answer(budget: Duration) -> Self {
        ChannelError::NoAnswer(format!(
            "The engine took the request but gave no answer within {} s, so it may have been \
             applied. Re-read with get_current_instrument or describe_instrument before retrying.",
            budget.as_secs_f32()
        ))
    }

    /// Whether this is the unreachable-engine case.
    pub fn is_unreachable(&self) -> bool {
        matches!(self, ChannelError::Unreachable(_))
    }
}

impl fmt::Display for ChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ChannelError::Unreachable(m) | ChannelError::NoAnswer(m) => write!(f, "{m}"),
            ChannelError::Channel(m) => write!(f, "engine channel error: {m}"),
            ChannelError::Protocol(m) => write!(f, "engine channel protocol error: {m}"),
        }
    }
}

impl std::error::Error for ChannelError {}

/// The outcome of an `open` that reached the engine — every arm is an answer, not a failure
/// (transport failures are [`ChannelError`]).
#[derive(Debug, Clone, PartialEq)]
pub enum SwapOutcome {
    /// The engine processed the open and returned its [`SwapReport`] (success or load-failure).
    Installed(SwapReport),
    /// The `expect` guard missed: nothing installed; the [`Conflict`] names what keeps playing.
    Conflict(Conflict),
    /// The host held the open to ask its user first: nothing installed; the [`HeldSwap`] names
    /// what keeps playing.
    Held(HeldSwap),
    /// The server would not install it (a name it does not offer, say): nothing installed. The
    /// message is the server's, verbatim.
    Refused(String),
}

/// The outcome of a `send` that reached the engine. Transport failures are [`ChannelError`].
#[derive(Debug, Clone, PartialEq)]
pub enum SendOutcome {
    /// The engine queued the whole batch for its next block.
    Queued,
    /// A string did not compile for the input it was sent to: nothing was queued.
    Refused(SendRefusal),
}

/// The outcome of a `call` that reached the engine — a verb that ran, or one the server refused to
/// run. Transport failures are [`ChannelError`].
#[derive(Debug, Clone, PartialEq)]
pub enum CallOutcome {
    /// The verb ran: its output (the verb's own output type, as JSON) and one-line summary.
    Called {
        output: serde_json::Value,
        summary: String,
    },
    /// The server refused to run it, with its reason verbatim.
    Refused(String),
}

/// The cause a [`Transport`] wraps a read failure in, via [`unanswered`], once the whole request
/// line was written: the request reached the engine, and only its answer is missing.
#[derive(Debug)]
pub struct Unanswered(io::Error);

impl fmt::Display for Unanswered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "no answer after the request was sent: {}", self.0)
    }
}

impl std::error::Error for Unanswered {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

/// Mark a read timeout that followed a fully written request line, so the channel answers
/// [`ChannelError::NoAnswer`] rather than [`ChannelError::Unreachable`].
pub fn unanswered(cause: io::Error) -> io::Error {
    io::Error::new(cause.kind(), Unanswered(cause))
}

/// The one thing a engine channel must be able to do: hand a request line to the engine and
/// return the response line.
///
/// **The injectable seam**, and deliberately the lowest one: everything above it — framing,
/// parsing, the unreachable/protocol split — is exercised rather than replaced by a test double.
/// Below it live the socket mechanics, which are the door's — the TCP one is behind the
/// `tcp` feature, and `reuben-mcp`'s tests drive it over a real socket.
pub trait Transport: Send + Sync + fmt::Debug {
    /// One request line out, one response line back. `read_timeout` is per-call because `ping`
    /// runs on a tighter budget than the other verbs. Any I/O failure — refused connect,
    /// unresolved address, timeout, or a peer that closed before answering — is an
    /// [`io::Error`]; the caller classifies it. A read that timed out after the whole line was
    /// written is returned through [`unanswered`].
    fn round_trip(&self, line: &str, read_timeout: Duration) -> io::Result<String>;

    /// The endpoint this transport dials, for `engine_status` and diagnostics.
    fn endpoint(&self) -> &str;

    /// A stream of the engine's [`Event`]s on a connection of its own, from the acknowledgement
    /// on. The iterator ends when the engine closes the stream, and after the first read error.
    /// A transport with no such stream answers [`io::ErrorKind::Unsupported`].
    fn subscribe(&self) -> io::Result<Box<dyn Iterator<Item = io::Result<Event>> + Send>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "this transport cannot stream events",
        ))
    }
}

/// The engine's events as a channel hands them: in the order the engine made them, ending when the
/// engine closes the stream or a read fails.
pub type Events = Box<dyn Iterator<Item = Event> + Send>;

/// One engine's engine channel: the NDJSON framing, the response-variant classification, and
/// the timeout policy, over the door's [`Transport`].
#[derive(Debug)]
pub struct Channel {
    transport: Box<dyn Transport>,
    read_timeout: Duration,
    /// The tighter read budget `ping` uses (its pong is immediate) — never longer than the general
    /// `read_timeout`, and capped at [`DEFAULT_PING_READ_TIMEOUT`]. See [`Self::ping`].
    ping_read_timeout: Duration,
    /// The longer read budget of `open` and `call`, which may plan a whole tree.
    work_read_timeout: Duration,
}

impl Channel {
    /// A channel over the door's transport, on the default per-verb read budgets.
    pub fn new(transport: impl Transport + 'static) -> Self {
        Self {
            work_read_timeout: DEFAULT_WORK_READ_TIMEOUT,
            ..Self::with_read_timeout(transport, DEFAULT_READ_TIMEOUT)
        }
    }

    /// A channel with one explicit read budget for every verb but `ping`, whose tighter one is
    /// derived from it.
    pub fn with_read_timeout(transport: impl Transport + 'static, read_timeout: Duration) -> Self {
        Self {
            transport: Box::new(transport),
            read_timeout,
            // The pong is immediate, so `ping` uses the tighter of the two budgets: never longer
            // than a caller's explicit read timeout (a deliberately tiny one still wins — the
            // wedged-server test relies on that), but capped at the ping default when it's generous.
            ping_read_timeout: read_timeout.min(DEFAULT_PING_READ_TIMEOUT),
            work_read_timeout: read_timeout,
        }
    }

    /// Follow the engine's changes: one [`Event`] per change from now on, on a connection of its
    /// own. Iterating blocks until the next one. A transport that cannot stream is a
    /// [`ChannelError::Channel`]; one that cannot connect is [`Unreachable`](ChannelError::Unreachable).
    pub fn subscribe(&self) -> Result<Events, ChannelError> {
        match self.transport.subscribe() {
            Ok(events) => Ok(Box::new(events.map_while(Result::ok))),
            Err(e) if e.kind() == io::ErrorKind::Unsupported => {
                Err(ChannelError::Channel(e.to_string()))
            }
            Err(e) => Err(ChannelError::unreachable(&e)),
        }
    }

    /// The endpoint this channel reaches.
    pub fn endpoint(&self) -> &str {
        self.transport.endpoint()
    }

    /// Liveness: `Ok(())` iff the channel answered [`Response::Pong`]. The only probe on the
    /// channel — every other verb acts and maps its own failure. An unanswered ping is
    /// [`Unreachable`](ChannelError::Unreachable): a ping applies nothing, and an engine that
    /// cannot answer it is not one to talk to.
    pub fn ping(&self) -> Result<(), ChannelError> {
        let answer = self
            .exchange_with(&Request::Ping, self.ping_read_timeout)
            .map_err(|e| match e {
                ChannelError::NoAnswer(_) => ChannelError::unreachable("no answer to a ping"),
                other => other,
            })?;
        match answer {
            Response::Pong => Ok(()),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("ping", "pong", &other)),
        }
    }

    /// Unload the open instrument and play the one `name` names, read through the server's store.
    /// An optional `expect` content-hash guard rejects the open on mismatch.
    pub fn open(&self, name: String, expect: Option<String>) -> Result<SwapOutcome, ChannelError> {
        match self.exchange_with(&Request::Open { name, expect }, self.work_read_timeout)? {
            Response::SwapReport(report) => Ok(SwapOutcome::Installed(report)),
            Response::Conflict(conflict) => Ok(SwapOutcome::Conflict(conflict)),
            Response::Held(held) => Ok(SwapOutcome::Held(held)),
            Response::Refused { message } => Ok(SwapOutcome::Refused(message)),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected(
                "open",
                "swap_report/conflict/held/refused",
                &other,
            )),
        }
    }

    /// Run one non-engine roster verb on the server, against its store.
    pub fn call(&self, tool: &str, args: serde_json::Value) -> Result<CallOutcome, ChannelError> {
        let request = Request::Call {
            tool: tool.to_string(),
            args,
        };
        match self.exchange_with(&request, self.work_read_timeout)? {
            Response::Called { output, summary } => Ok(CallOutcome::Called { output, summary }),
            Response::Refused { message } => Ok(CallOutcome::Refused(message)),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("call", "called/refused", &other)),
        }
    }

    /// The names the server offers of one kind, sorted.
    pub fn list(&self, what: ListWhat) -> Result<Vec<String>, ChannelError> {
        match self.exchange(&Request::List { what })? {
            Response::Listed { names } => Ok(names),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("list", "listed", &other)),
        }
    }

    /// Every instrument the server offers, read, and where a new one may be created.
    pub fn library(&self) -> Result<Library, ChannelError> {
        match self.exchange_with(&Request::Library, self.work_read_timeout)? {
            Response::Library(library) => Ok(library),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("library", "library", &other)),
        }
    }

    /// Read what plays, by source and content hash. Never the document itself.
    pub fn get_document(&self) -> Result<DocumentSnapshot, ChannelError> {
        match self.exchange(&Request::GetDocument)? {
            Response::Document(snapshot) => Ok(snapshot),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("get_document", "document", &other)),
        }
    }

    /// Audition a batch of control values on the running engine.
    ///
    /// [`SendOutcome::Queued`] means "received and queued", NOT "applied": a message whose address
    /// routes nowhere is dropped at the engine's ingress. [`SendOutcome::Refused`] is text aimed at
    /// an input that compiles it that did not compile, and nothing was queued. An empty or
    /// over-long batch is refused by the engine as a [`Channel`](ChannelError::Channel) error
    /// rather than acked.
    pub fn send(&self, messages: Vec<ControlMessage>) -> Result<SendOutcome, ChannelError> {
        match self.exchange(&Request::Send { messages })? {
            Response::Sent => Ok(SendOutcome::Queued),
            Response::SendRefused(refusal) => Ok(SendOutcome::Refused(refusal)),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("send", "sent/send_refused", &other)),
        }
    }

    /// Inject one translated MIDI message into the engine's MIDI router; see
    /// [`Request::SendMidi`]. [`SendOutcome::Queued`] means routed as the open instrument's MIDI map
    /// says, which may be nowhere.
    pub fn send_midi(
        &self,
        address: String,
        args: Vec<ControlArg>,
    ) -> Result<SendOutcome, ChannelError> {
        match self.exchange(&Request::SendMidi { address, args })? {
            Response::Sent => Ok(SendOutcome::Queued),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("send_midi", "sent", &other)),
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

    /// Read the engine's levels at the device.
    pub fn get_meters(&self) -> Result<MetersReport, ChannelError> {
        match self.exchange(&Request::GetMeters)? {
            Response::Meters(report) => Ok(report),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("get_meters", "meters", &other)),
        }
    }

    /// Choose which nodes' levels [`get_meters`](Self::get_meters) reports, and read back the paths
    /// that resolved. Over [`MAX_PROBES`] is refused here, saving the round trip; the server
    /// refuses it too.
    pub fn set_probes(&self, paths: Vec<Vec<String>>) -> Result<Vec<Vec<String>>, ChannelError> {
        if paths.len() > MAX_PROBES {
            return Err(ChannelError::Channel(over_long_probe_set_refusal()));
        }
        match self.exchange(&Request::SetProbes { paths })? {
            Response::Probes { resolved } => Ok(resolved),
            Response::Error { message } => Err(ChannelError::Channel(message)),
            other => Err(unexpected("set_probes", "probes", &other)),
        }
    }

    /// One request → one response on the general budget, which the verbs that only read or queue
    /// use.
    fn exchange(&self, request: &Request) -> Result<Response, ChannelError> {
        self.exchange_with(request, self.read_timeout)
    }

    /// [`exchange`](Self::exchange) with an explicit per-call read/write budget.
    ///
    /// The policy layer, above the socket: framing out, one line back, and the split between a
    /// transport failure (unreachable), a request whose answer did not come (no answer), and a
    /// line that came back unparseable (protocol).
    fn exchange_with(
        &self,
        request: &Request,
        read_timeout: Duration,
    ) -> Result<Response, ChannelError> {
        let line = self
            .transport
            .round_trip(&request.to_ndjson(), read_timeout)
            .map_err(|e| match e.kind() {
                // The bytes moved but no line could be framed from them: an over-long or
                // non-UTF-8 line, from an engine that is plainly reachable.
                io::ErrorKind::InvalidData => ChannelError::Protocol(e.to_string()),
                _ if e.get_ref().is_some_and(|cause| cause.is::<Unanswered>()) => {
                    ChannelError::no_answer(read_timeout)
                }
                _ => ChannelError::unreachable(e),
            })?;
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

    use serde_json::json;

    #[derive(Debug)]
    struct Fake {
        reply: String,
        asked: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl Transport for Fake {
        fn round_trip(&self, line: &str, _read_timeout: Duration) -> io::Result<String> {
            self.asked.lock().unwrap().push(line.to_string());
            Ok(self.reply.clone())
        }

        fn endpoint(&self) -> &str {
            "fake"
        }
    }

    fn answering(reply: &str) -> (Channel, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let fake = Fake {
            reply: reply.to_string(),
            asked: asked.clone(),
        };
        (Channel::new(fake), asked)
    }

    #[test]
    fn a_ping_the_server_refuses_carries_its_message() {
        let (channel, _) =
            answering(r#"{"reply":"error","message":"the engine refused this token"}"#);
        let refused = channel.ping().expect_err("refused");
        assert!(
            matches!(&refused, ChannelError::Channel(m) if m == "the engine refused this token"),
            "{refused:?}"
        );
    }

    #[test]
    fn call_relays_called_output_and_summary() {
        let (channel, asked) = answering(r#"{"reply":"called","output":{"x":1},"summary":"s"}"#);
        let args = json!({"source": "a.json"});
        let outcome = channel.call("describe_instrument", args.clone()).unwrap();
        assert_eq!(
            outcome,
            CallOutcome::Called {
                output: json!({"x": 1}),
                summary: "s".to_string()
            }
        );
        let expected = Request::Call {
            tool: "describe_instrument".to_string(),
            args,
        }
        .to_ndjson();
        assert_eq!(asked.lock().unwrap().as_slice(), [expected]);
    }

    #[test]
    fn call_relays_a_refusal_verbatim() {
        let (channel, _) = answering(r#"{"reply":"refused","message":"m"}"#);
        let outcome = channel.call("nope", json!({})).unwrap();
        assert_eq!(outcome, CallOutcome::Refused("m".to_string()));
    }

    #[test]
    fn open_maps_refused_held_conflict_and_report() {
        let (channel, asked) = answering(r#"{"reply":"refused","message":"not offered"}"#);
        let outcome = channel.open("a.json".to_string(), Some("h".to_string()));
        assert_eq!(
            outcome.unwrap(),
            SwapOutcome::Refused("not offered".to_string())
        );
        let expected = Request::Open {
            name: "a.json".to_string(),
            expect: Some("h".to_string()),
        }
        .to_ndjson();
        assert_eq!(asked.lock().unwrap().as_slice(), [expected]);

        let held = HeldSwap {
            requested: "a.json".to_string(),
            content_hash: "h".to_string(),
            playing: None,
        };
        let conflict = Conflict {
            expected: "e".to_string(),
            actual: "a".to_string(),
        };
        let report = SwapReport::rejected("h".to_string());
        for (response, want) in [
            (Response::Held(held.clone()), SwapOutcome::Held(held)),
            (
                Response::Conflict(conflict.clone()),
                SwapOutcome::Conflict(conflict),
            ),
            (
                Response::SwapReport(report.clone()),
                SwapOutcome::Installed(report),
            ),
        ] {
            let (channel, _) = answering(&response.to_ndjson());
            assert_eq!(channel.open("a.json".to_string(), None).unwrap(), want);
        }
    }

    /// A transport whose every exchange fails with what `fail` makes, recording each read budget.
    #[derive(Debug)]
    struct Failing {
        fail: fn() -> io::Error,
        budgets: std::sync::Arc<std::sync::Mutex<Vec<Duration>>>,
    }

    impl Transport for Failing {
        fn round_trip(&self, _line: &str, read_timeout: Duration) -> io::Result<String> {
            self.budgets.lock().unwrap().push(read_timeout);
            Err((self.fail)())
        }

        fn endpoint(&self) -> &str {
            "failing"
        }
    }

    fn failing(
        fail: fn() -> io::Error,
    ) -> (Channel, std::sync::Arc<std::sync::Mutex<Vec<Duration>>>) {
        let budgets = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let transport = Failing {
            fail,
            budgets: budgets.clone(),
        };
        (Channel::new(transport), budgets)
    }

    fn read_timed_out() -> io::Error {
        unanswered(io::Error::new(io::ErrorKind::WouldBlock, "read timed out"))
    }

    #[test]
    fn a_request_written_but_unanswered_is_no_answer_not_unreachable() {
        let (channel, _) = failing(read_timed_out);
        let errors = [
            channel.call("add_instrument_node", json!({})).unwrap_err(),
            channel.open("a.json".to_string(), None).unwrap_err(),
            channel.send(Vec::new()).unwrap_err(),
        ];
        for err in errors {
            assert!(matches!(err, ChannelError::NoAnswer(_)), "{err:?}");
            assert!(!err.is_unreachable());
            let shown = err.to_string();
            assert!(shown.contains("may have been applied"), "{shown}");
            assert!(!shown.contains("reuben play"), "{shown}");
        }
    }

    #[test]
    fn a_failed_connect_stays_unreachable() {
        let (channel, _) = failing(|| io::Error::new(io::ErrorKind::TimedOut, "connect timed out"));
        let err = channel.call("add_instrument_node", json!({})).unwrap_err();
        assert!(err.is_unreachable(), "{err:?}");
    }

    #[test]
    fn an_unanswered_ping_is_unreachable() {
        let (channel, _) = failing(read_timed_out);
        assert!(channel.ping().unwrap_err().is_unreachable());
    }

    #[test]
    fn open_and_call_wait_longer_than_the_verbs_that_only_read() {
        let (channel, budgets) = failing(read_timed_out);
        let _ = channel.ping();
        let _ = channel.get_document();
        let _ = channel.call("describe_instrument", json!({}));
        let _ = channel.open("a.json".to_string(), None);
        assert_eq!(
            budgets.lock().unwrap().as_slice(),
            [
                DEFAULT_PING_READ_TIMEOUT,
                DEFAULT_READ_TIMEOUT,
                DEFAULT_WORK_READ_TIMEOUT,
                DEFAULT_WORK_READ_TIMEOUT,
            ]
        );
    }

    #[test]
    fn list_returns_the_names_in_order() {
        let (channel, asked) = answering(r#"{"reply":"listed","names":["a.json","b.json"]}"#);
        let names = channel.list(ListWhat::Instruments).unwrap();
        assert_eq!(names, ["a.json", "b.json"]);
        let expected = Request::List {
            what: ListWhat::Instruments,
        }
        .to_ndjson();
        assert_eq!(asked.lock().unwrap().as_slice(), [expected]);
    }

    #[test]
    fn library_decodes_its_answer() {
        let (channel, asked) = answering(
            r#"{"reply":"library","instruments":[{"name":"a.json","reads":["a.json"]}],"dirs":["x/"]}"#,
        );
        let library = channel.library().unwrap();
        assert_eq!(library.instruments[0].name, "a.json");
        assert_eq!(library.dirs, ["x/"]);
        assert_eq!(
            asked.lock().unwrap().as_slice(),
            [Request::Library.to_ndjson()]
        );
    }
}
