//! The structure channel: a loopback-TCP / NDJSON server (ADR-0046 §8).
//!
//! This is the engine-side half of the sidecar↔engine control channel ADR-0044 delegated to
//! ADR-0046: **TCP on `127.0.0.1`** (loopback-only — structure edits are more powerful than
//! control, unlike OSC's `0.0.0.0:9000`), carrying **newline-delimited JSON**, one
//! [`Response`] per [`Request`] in order. A thread in `reuben play` owns it; the client lives
//! in `reuben-mcp`. Zero new dependencies beyond std, cross-platform, netcat-debuggable.
//!
//! M1 wires the three **non-mutating** verbs:
//! - [`Request::Ping`] → [`Response::Pong`] — liveness of the channel itself (ADR-0044 §2).
//! - [`Request::GetDocument`] → the retained canonical document + its
//!   [`content_hash`](reuben_core::content_hash) (ADR-0046 §7/§9). The document never changes
//!   for the life of the process — `swap` (the only thing that would change it) is stubbed
//!   below.
//! - [`Request::GetDiagnostics`] → a [`DiagnosticsReport`] built from a live [`Snapshot`] of
//!   the [`Diagnostics`] `audio::start` owns (ADR-0038 §9 / ADR-0048 §6). **RT-safety:** the
//!   snapshot is [`Diagnostics::snapshot`]'s four `Relaxed` atomic loads into an owned copy —
//!   the query thread never forces the audio callback to synchronize.
//!
//! [`Request::Swap`] returns a not-yet-implemented [`Response::Error`]: the real restart-swap
//! install path lands in issue #317, so this channel does not mutate the engine yet.
//!
//! reuben-native stays **tokio-free** (ADR-0044 §3 fence): the server is a dedicated std
//! thread doing blocking line I/O, never an async runtime.

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use reuben_core::coordinator::{DiagnosticsReport, Request, Response};
use reuben_core::{content_hash, NormalizedDoc};

use crate::diagnostics::{Diagnostics, Snapshot};

/// The `swap` stub message (ADR-0046 §8's fourth verb, deferred to issue #317): the channel
/// answers, but nothing installs — distinct from a mutating swap that fails.
const SWAP_UNIMPLEMENTED: &str =
    "swap is not yet implemented on the structure channel (the restart-swap install path lands \
     in issue #317)";

/// How long the accept loop sleeps between polls of its shutdown flag while idle. The listener
/// is non-blocking so the loop can observe [`StructureServer::shutdown`] without a client ever
/// connecting; this cadence is the shutdown latency, not a hot path (never the audio thread).
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// Everything the structure server answers with, cheap to clone (`Arc`-backed) so every
/// connection-handler thread holds its own handle.
///
/// The document and its hash are **retained and immutable** for the life of the process: they
/// are the canonical instrument `play` loaded (ADR-0046 §7), and only `swap` — stubbed in M1 —
/// would replace them. The diagnostics `Arc` is the live counter surface both audio callbacks
/// feed; each `get_diagnostics` takes a fresh [`Snapshot`] of it.
#[derive(Clone)]
pub struct StructureState {
    document: Arc<Value>,
    content_hash: Arc<str>,
    diagnostics: Arc<Diagnostics>,
}

impl StructureState {
    /// Retain a pre-serialized document + hash alongside the live diagnostics. The document is
    /// the canonical instrument as a JSON value; `content_hash` is its
    /// [`content_hash`](reuben_core::content_hash) token (ADR-0046 §9).
    pub fn new(document: Value, content_hash: String, diagnostics: Arc<Diagnostics>) -> Self {
        Self {
            document: Arc::new(document),
            content_hash: content_hash.into(),
            diagnostics,
        }
    }

    /// Retain the canonical [`NormalizedDoc`] `play` loaded: serialize it once to a JSON value
    /// and compute its content hash here, so every `get_document` is a cheap `Arc` read. The
    /// value serialized is the same [`InstrumentDoc`](reuben_core::InstrumentDoc) the hash is
    /// taken over, so the pair a client reads is self-consistent (ADR-0046 §9).
    pub fn from_doc(doc: &NormalizedDoc, diagnostics: Arc<Diagnostics>) -> Self {
        let document =
            serde_json::to_value(&**doc).expect("canonical instrument document serializes to JSON");
        Self::new(document, content_hash(doc), diagnostics)
    }
}

/// Map a diagnostics [`Snapshot`] (reuben-native's counter surface) to the wire
/// [`DiagnosticsReport`] (reuben-core's contract type, ADR-0038 §9 / ADR-0048 §6).
///
/// The two structs are duplicated across the crate boundary with no shared definition, so they
/// could silently drift. The **exhaustive destructure** below is the compile-time coupling that
/// prevents it: add a counter to [`Snapshot`] and this `let Snapshot { .. }` stops compiling
/// (non-exhaustive, no `..`); add a field to [`DiagnosticsReport`] and the struct literal stops
/// compiling (missing field). Either drift is a build break here, not a runtime surprise
/// (follow-up from #310's review). The behavioral half — that each counter maps to the *right*
/// field — is [`tests::diagnostics_report_maps_every_counter_field_for_field`].
pub fn diagnostics_report(snapshot: &Snapshot) -> DiagnosticsReport {
    let Snapshot {
        output_xruns,
        input_ring_underruns,
        input_ring_overruns,
        input_ring_producer_drops,
    } = *snapshot;
    DiagnosticsReport {
        output_xruns,
        input_ring_underruns,
        input_ring_overruns,
        input_ring_producer_drops,
    }
}

/// Dispatch one parsed request line to its response (ADR-0046 §8). Pure over [`StructureState`]
/// so it is unit-testable without a socket; the connection loop only frames it.
///
/// An unreadable line is a channel-level [`Response::Error`] (ADR-0048 §3: distinct from a
/// domain answer that reports failure), so a malformed request still gets exactly one framed
/// reply and the one-response-per-request invariant holds.
fn dispatch(state: &StructureState, line: &str) -> Response {
    match Request::from_ndjson(line) {
        Ok(Request::Ping) => Response::Pong,
        Ok(Request::GetDocument) => Response::Document {
            document: (*state.document).clone(),
            content_hash: state.content_hash.to_string(),
        },
        // RT-safe read: four `Relaxed` loads into an owned copy off this (non-audio) thread.
        Ok(Request::GetDiagnostics) => {
            Response::Diagnostics(diagnostics_report(&state.diagnostics.snapshot()))
        }
        Ok(Request::Swap { .. }) => Response::Error {
            message: SWAP_UNIMPLEMENTED.to_string(),
        },
        Err(e) => Response::Error {
            message: format!("unreadable request: {e}"),
        },
    }
}

/// A running loopback structure server: a std thread accepting connections, one handler thread
/// per connection, all joined on [`shutdown`](Self::shutdown) (or `Drop`). Holds no audio state
/// — it only reads the shared [`StructureState`] — so it starts and stops independently of the
/// audio device, which is what lets it be exercised end-to-end in tests.
pub struct StructureServer {
    local_addr: SocketAddr,
    shutdown: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
}

impl StructureServer {
    /// Bind a loopback TCP listener and start serving. Pass `127.0.0.1:0` to let the OS assign
    /// an ephemeral port (read it back with [`local_addr`](Self::local_addr)); this is how tests
    /// avoid port collisions. Binding a non-loopback address is the caller's mistake — the
    /// structure channel must not be network-exposed (ADR-0046 §8) — but not enforced here.
    pub fn bind<A: ToSocketAddrs>(addr: A, state: StructureState) -> std::io::Result<Self> {
        let listener = TcpListener::bind(addr)?;
        // Non-blocking so the accept loop can poll its shutdown flag with no client connected.
        listener.set_nonblocking(true)?;
        let local_addr = listener.local_addr()?;
        let shutdown = Arc::new(AtomicBool::new(false));
        let accept = {
            let shutdown = Arc::clone(&shutdown);
            std::thread::Builder::new()
                .name("structure-accept".to_string())
                .spawn(move || accept_loop(listener, state, shutdown))
                .expect("spawn structure-accept thread")
        };
        Ok(Self {
            local_addr,
            shutdown,
            accept: Some(accept),
        })
    }

    /// The bound local address — the ephemeral port the OS assigned when binding to `:0`.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Stop accepting, wake and join every connection handler, then join the accept thread.
    /// Returns only once all structure threads have exited — the clean, joinable stop that
    /// replaces `play`'s park-forever loop. Idempotent with `Drop`.
    pub fn shutdown(mut self) {
        self.signal_and_join();
    }

    fn signal_and_join(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }
}

impl Drop for StructureServer {
    fn drop(&mut self) {
        self.signal_and_join();
    }
}

/// The accept thread: poll the non-blocking listener until the shutdown flag flips, spawning a
/// blocking handler per connection. On shutdown, wake each live handler (a socket `shutdown`
/// unblocks its `read_line`) and join it, so no idle client keeps the process alive — the exact
/// hang `play`'s old `loop { thread::park() }` had, removed.
fn accept_loop(listener: TcpListener, state: StructureState, shutdown: Arc<AtomicBool>) {
    // (join handle, a clone of the socket used only to wake the handler at shutdown).
    let mut handlers: Vec<(JoinHandle<()>, Option<TcpStream>)> = Vec::new();
    while !shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _peer)) => {
                // The accepted socket inherits the listener's non-blocking mode on some
                // platforms; the handler wants blocking reads (clean NDJSON framing — a
                // partial-line timeout would corrupt the next request), so force it.
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_nodelay(true);
                let wake = stream.try_clone().ok();
                let handler_state = state.clone();
                let handler_shutdown = Arc::clone(&shutdown);
                let handle = std::thread::Builder::new()
                    .name("structure-conn".to_string())
                    .spawn(move || handle_connection(stream, handler_state, handler_shutdown))
                    .expect("spawn structure-conn thread");
                handlers.push((handle, wake));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            // A transient accept error is not fatal to the channel; back off and re-check the
            // shutdown flag on the next turn rather than spin.
            Err(_) => std::thread::sleep(ACCEPT_POLL),
        }
    }
    for (handle, wake) in handlers {
        if let Some(wake) = wake {
            let _ = wake.shutdown(Shutdown::Both);
        }
        let _ = handle.join();
    }
}

/// Serve one connection: read one [`Request`] per line, write one [`Response`] per line, in
/// order (ADR-0046 §8), until the client closes or shutdown wakes the blocked read. Blocking
/// reads keep the framing exact; a blank line is framing noise, not a request, so it draws no
/// response.
fn handle_connection(stream: TcpStream, state: StructureState, shutdown: Arc<AtomicBool>) {
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        // Can't split the socket into independent read/write halves; nothing to serve on.
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            // EOF: client closed, or a shutdown `Shutdown::Both` woke us.
            Ok(0) => break,
            Ok(_) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                if line.trim().is_empty() {
                    continue;
                }
                let response = dispatch(&state, &line);
                if writer.write_all(response.to_ndjson().as_bytes()).is_err() {
                    break;
                }
                if writer.flush().is_err() {
                    break;
                }
            }
            // A read error (including the socket shutdown that wakes us) ends the connection.
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reuben_core::coordinator::DocSource;
    use reuben_core::Registry;

    fn doc() -> NormalizedDoc {
        NormalizedDoc::from_json(
            r#"{"format_version":3,"instrument":"t",
                "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
                "nodes":[{"type":"oscillator","address":"/osc"}]}"#,
            &Registry::builtin(),
            None,
        )
        .expect("test instrument normalizes")
    }

    fn state(diagnostics: Arc<Diagnostics>) -> StructureState {
        StructureState::from_doc(&doc(), diagnostics)
    }

    #[test]
    fn diagnostics_report_maps_every_counter_field_for_field() {
        // Distinct values per counter so a mis-wire (mapping overruns to underruns, say) is
        // caught — equal values would let a swapped pair pass. Frame counts are deliberately
        // different magnitudes; the xrun count is an event count.
        let d = Diagnostics::new();
        d.record_output_xrun();
        d.record_output_xrun();
        d.record_output_xrun(); // 3 events
        d.record_input_ring_underrun_frames(11);
        d.record_input_ring_overrun_frames(22);
        d.record_input_ring_producer_drop_frames(33);
        let report = diagnostics_report(&d.snapshot());
        assert_eq!(report.output_xruns, 3);
        assert_eq!(report.input_ring_underruns, 11);
        assert_eq!(report.input_ring_overruns, 22);
        assert_eq!(report.input_ring_producer_drops, 33);
    }

    #[test]
    fn ping_is_answered_with_pong() {
        let resp = dispatch(&state(Diagnostics::new()), &Request::Ping.to_ndjson());
        assert_eq!(resp, Response::Pong);
    }

    #[test]
    fn get_document_returns_the_retained_doc_and_its_hash() {
        let doc = doc();
        let resp = dispatch(
            &StructureState::from_doc(&doc, Diagnostics::new()),
            &Request::GetDocument.to_ndjson(),
        );
        match resp {
            Response::Document {
                document,
                content_hash: hash,
            } => {
                assert_eq!(document, serde_json::to_value(&*doc).unwrap());
                assert_eq!(hash, content_hash(&doc));
                assert_eq!(document["instrument"], serde_json::json!("t"));
            }
            other => panic!("expected Document, got {other:?}"),
        }
    }

    #[test]
    fn get_diagnostics_reads_the_live_counters() {
        let diagnostics = Diagnostics::new();
        let state = state(Arc::clone(&diagnostics));
        // Fresh: zeroed.
        assert_eq!(
            dispatch(&state, &Request::GetDiagnostics.to_ndjson()),
            Response::Diagnostics(DiagnosticsReport::default())
        );
        // A later bump is visible on the next query — proving the Arc is read, not a frozen
        // copy taken at construction.
        diagnostics.record_output_xrun();
        assert_eq!(
            dispatch(&state, &Request::GetDiagnostics.to_ndjson()),
            Response::Diagnostics(DiagnosticsReport {
                output_xruns: 1,
                ..DiagnosticsReport::default()
            })
        );
    }

    #[test]
    fn swap_is_rejected_as_not_yet_implemented() {
        let req = Request::Swap {
            source: DocSource::Path("instruments/warm-pad.json".to_string()),
            expect: None,
        };
        match dispatch(&state(Diagnostics::new()), &req.to_ndjson()) {
            Response::Error { message } => {
                assert!(
                    message.contains("not yet implemented") && message.contains("#317"),
                    "swap stub must name itself unimplemented and point at #317: {message:?}"
                );
            }
            other => panic!("swap must return Error, got {other:?}"),
        }
    }

    #[test]
    fn an_unreadable_line_is_a_channel_error() {
        match dispatch(&state(Diagnostics::new()), "{not json}\n") {
            Response::Error { message } => assert!(message.contains("unreadable request")),
            other => panic!("a malformed request must return Error, got {other:?}"),
        }
    }
}
