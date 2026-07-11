//! The structure channel: a loopback-TCP / NDJSON server (ADR-0046 §8).
//!
//! This is the engine-side half of the sidecar↔engine control channel ADR-0044 delegated to
//! ADR-0046: **TCP on `127.0.0.1`** (loopback-only — structure edits are more powerful than
//! control, unlike OSC's `0.0.0.0:9000`), carrying **newline-delimited JSON**, one
//! [`Response`] per [`Request`] in order. A thread in `reuben play` owns it; the client lives
//! in `reuben-mcp`. Zero new dependencies beyond std, cross-platform, netcat-debuggable.
//!
//! M1 wires all four verbs:
//! - [`Request::Ping`] → [`Response::Pong`] — liveness of the channel itself (ADR-0044 §2).
//! - [`Request::GetDocument`] → the retained canonical document + its
//!   [`content_hash`](reuben_core::content_hash) (ADR-0046 §7/§9). It changes only when a
//!   [`Request::Swap`] installs a new document.
//! - [`Request::GetDiagnostics`] → a [`DiagnosticsReport`] built from a live [`Snapshot`] of
//!   the [`Diagnostics`] `audio::start` owns (ADR-0038 §9 / ADR-0048 §6). **RT-safety:** the
//!   snapshot is [`Diagnostics::snapshot`]'s four `Relaxed` atomic loads into an owned copy —
//!   the query thread never forces the audio callback to synchronize.
//! - [`Request::Swap`] → a **restart-swap** (ADR-0046 §10): validate the new document through
//!   the single loader authority (ADR-0045 §3), and on success stop-the-world restart the
//!   audio streams and retain the new document. The validate → report → doc/hash-update →
//!   `expect` arbitration all live here on the server thread and are device-independent; the
//!   actual stream teardown/reopen is delegated to an injected [`SwapInstaller`] (`play`
//!   supplies the real one, a test supplies a no-op), so the swap **logic** is exercised
//!   headlessly (ADR-0053 §4).
//!
//! reuben-native stays **tokio-free** (ADR-0044 §3 fence): the server is a dedicated std
//! thread doing blocking line I/O, never an async runtime.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::Value;

use reuben_core::coordinator::{DiagnosticsReport, DocSource, Request, Response};
use reuben_core::introspect::validate;
use reuben_core::resources::ResourceResolver;
use reuben_core::{content_hash, Diag, DiffSummary, NormalizedDoc, Registry, SwapReport};

use crate::diagnostics::{Diagnostics, Snapshot};
use crate::resources::FsResolver;

/// How long the accept loop sleeps between polls of its shutdown flag while idle. The listener
/// is non-blocking so the loop can observe [`StructureServer::shutdown`] without a client ever
/// connecting; this cadence is the shutdown latency, not a hot path (never the audio thread).
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// The device-side effect of a validated swap (ADR-0046 §10 stop-the-world restart): stop the
/// live cpal streams and reopen against the new document.
///
/// This is the **device seam**. The handler ([`handle_swap`]) does everything device-free —
/// arbitration, validation, report shaping, doc/hash install — and calls `restart` only once
/// the document has passed the loader authority (retain-prior: a validation failure never
/// reaches here, so the old engine keeps playing). `play` injects the real restart (drop the
/// old [`Streams`](crate::audio::Streams), `audio::start` the new); a test injects a no-op, so
/// the swap **logic** runs with no audio device (ADR-0053 §4).
///
/// `Send + Sync` because a connection-handler thread calls it. A returned `Err(message)` is a
/// **device/stream fault** (the document already validated) surfaced as a channel-level
/// [`Response::Error`] (ADR-0048 §3).
pub trait SwapInstaller: Send + Sync {
    /// Restart audio onto the validated `json` (resolved through `resolver`). Called on a
    /// connection-handler thread; the real implementation forwards to `play`'s owning thread,
    /// which is the single owner of every cpal `Stream` (see the module note on race-freedom).
    fn restart(&self, json: &str, resolver: FsResolver) -> Result<(), String>;
}

/// The default installer for a state built without [`with_installer`](StructureState::with_installer):
/// it performs no device restart and reports success, so the swap *logic* (validate → report,
/// doc/hash update, `expect` arbitration) runs headlessly — the device-independent seam the
/// integration and unit tests drive (ADR-0053 §4). Production always overrides it.
struct NoopInstaller;

impl SwapInstaller for NoopInstaller {
    fn restart(&self, _json: &str, _resolver: FsResolver) -> Result<(), String> {
        Ok(())
    }
}

/// A swappable pointer to the live [`Diagnostics`] surface. A restart-swap opens a fresh
/// [`Streams`](crate::audio::Streams) with a fresh `Diagnostics` (that is `audio::start`'s
/// contract); [`replace`](Self::replace) points this handle at the new counters so
/// `get_diagnostics` tracks the current session rather than the retired one. Cold path only —
/// the structure thread reads it, never the audio callback.
#[derive(Clone)]
pub struct DiagnosticsHandle(Arc<Mutex<Arc<Diagnostics>>>);

impl DiagnosticsHandle {
    /// Wrap the initial session's counters.
    pub fn new(diagnostics: Arc<Diagnostics>) -> Self {
        Self(Arc::new(Mutex::new(diagnostics)))
    }

    /// The current session's counters (a cheap `Arc` clone under a cold lock).
    pub fn current(&self) -> Arc<Diagnostics> {
        Arc::clone(&self.0.lock().expect("diagnostics handle poisoned"))
    }

    /// Point the handle at a new session's counters after a restart-swap.
    pub fn replace(&self, diagnostics: Arc<Diagnostics>) {
        *self.0.lock().expect("diagnostics handle poisoned") = diagnostics;
    }
}

/// The retained canonical document + its content hash (ADR-0046 §7/§9), behind one lock so the
/// `expect`-compare and the install are a single atomic critical section (a correct
/// compare-and-swap) and concurrent swaps can't interleave.
struct Canonical {
    document: Value,
    content_hash: String,
}

/// Resolver anchoring for a swap's document (ADR-0046 §8). A by-value document resolves its
/// resource paths against `base_dir` (the directory the *initial* instrument loaded from); a
/// by-path document anchors at its own file's directory (like `read_instrument`). Both fall
/// back to `root` (the library instrument-root) when set.
struct ResolveConfig {
    base_dir: PathBuf,
    root: Option<PathBuf>,
}

/// Everything the structure server answers with, cheap to clone (`Arc`-backed) so every
/// connection-handler thread holds its own handle.
///
/// The canonical document + hash are **mutable** now (behind a lock): `swap` replaces them
/// (ADR-0046 §10), and every `get_document` / `expect` reads the current pair. The diagnostics
/// handle points at the live counter surface; a restart-swap re-points it. The `installer` is
/// the device seam (see [`SwapInstaller`]); `resolve` anchors a swapped document's resources.
#[derive(Clone)]
pub struct StructureState {
    canonical: Arc<Mutex<Canonical>>,
    diagnostics: DiagnosticsHandle,
    installer: Arc<dyn SwapInstaller>,
    resolve: Arc<ResolveConfig>,
}

impl StructureState {
    /// Retain a pre-serialized document + hash alongside the live diagnostics. The document is
    /// the canonical instrument as a JSON value; `content_hash` is its
    /// [`content_hash`](reuben_core::content_hash) token (ADR-0046 §9). Built with a no-op
    /// [`SwapInstaller`] and a `.`-anchored resolver — production wires the real device restart
    /// with [`with_installer`](Self::with_installer) and the real anchoring with
    /// [`with_resolve`](Self::with_resolve).
    pub fn new(document: Value, content_hash: String, diagnostics: Arc<Diagnostics>) -> Self {
        Self {
            canonical: Arc::new(Mutex::new(Canonical {
                document,
                content_hash,
            })),
            diagnostics: DiagnosticsHandle::new(diagnostics),
            installer: Arc::new(NoopInstaller),
            resolve: Arc::new(ResolveConfig {
                base_dir: PathBuf::from("."),
                root: None,
            }),
        }
    }

    /// Retain the canonical [`NormalizedDoc`] `play` loaded: serialize it once to a JSON value
    /// and compute its content hash here. The value serialized is the same
    /// [`InstrumentDoc`](reuben_core::InstrumentDoc) the hash is taken over, so the pair a
    /// client reads is self-consistent (ADR-0046 §9).
    pub fn from_doc(doc: &NormalizedDoc, diagnostics: Arc<Diagnostics>) -> Self {
        let document =
            serde_json::to_value(&**doc).expect("canonical instrument document serializes to JSON");
        Self::new(document, content_hash(doc), diagnostics)
    }

    /// Wire the real device restart (`play`'s owning-thread installer). Without this a swap
    /// updates the retained document but restarts no audio (the headless test seam).
    pub fn with_installer(mut self, installer: Arc<dyn SwapInstaller>) -> Self {
        self.installer = installer;
        self
    }

    /// Anchor a swapped document's resource resolution (ADR-0046 §8): `base_dir` for by-value
    /// documents, `root` as the library-root fallback for both.
    pub fn with_resolve(mut self, base_dir: PathBuf, root: Option<PathBuf>) -> Self {
        self.resolve = Arc::new(ResolveConfig { base_dir, root });
        self
    }

    /// The swappable diagnostics handle, so `play`'s owning thread can re-point it at a fresh
    /// session's counters after a restart-swap.
    pub fn diagnostics_handle(&self) -> DiagnosticsHandle {
        self.diagnostics.clone()
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
        Ok(Request::GetDocument) => {
            let canonical = state.canonical.lock().expect("canonical mutex poisoned");
            Response::Document {
                document: canonical.document.clone(),
                content_hash: canonical.content_hash.clone(),
            }
        }
        // RT-safe read: four `Relaxed` loads into an owned copy off this (non-audio) thread.
        Ok(Request::GetDiagnostics) => {
            Response::Diagnostics(diagnostics_report(&state.diagnostics.current().snapshot()))
        }
        Ok(Request::Swap { source, expect }) => handle_swap(state, source, expect),
        Err(e) => Response::Error {
            message: format!("unreadable request: {e}"),
        },
    }
}

/// The restart-swap install path (ADR-0046 §10), device-free up to the [`SwapInstaller`] call.
///
/// The whole swap runs under the canonical lock so the `expect`-compare and the install are one
/// atomic critical section (ADR-0046 §9's compare-and-swap) — concurrent swaps from multiple
/// connections serialize, and neither `get_document` nor another swap sees a half-installed
/// pair. In order:
///
/// 1. **Arbitration** (ADR-0046 §9 / ADR-0044 §4): a stale `expect` rejects with the real
///    installed hash as [`Response::Conflict`] and does **not** restart. Absent `expect` is
///    last-write-wins.
/// 2. **Resolve** the [`DocSource`] to `(json, resolver)` — inline JSON, or a file read + a
///    resolver anchored at its directory (ADR-0046 §8). A read failure is a rejected
///    [`SwapReport`] (no install, prior retained), not a channel `Error`.
/// 3. **Validate** through the single loader authority (ADR-0045 §3). Any load/plan error
///    aborts with `ok: false`, the errors, and the **prior** hash — the old engine keeps
///    playing (retain-prior, ADR-0046 §10). Warnings pass through (ADR-0016).
/// 4. **Install**: hand the validated document to the device seam ([`SwapInstaller::restart`]).
///    A failure here is post-validation, i.e. a device/stream fault — a channel `Error`, with
///    the prior doc/hash left intact.
/// 5. **Commit** the new document + hash so `get_document` and a later `expect` see it, and
///    answer with the success [`SwapReport`] — `survived: 0` under M1's all-cold restart
///    (ADR-0046 §10), the diff naming what reset/added/removed.
fn handle_swap(state: &StructureState, source: DocSource, expect: Option<String>) -> Response {
    let mut canonical = state.canonical.lock().expect("canonical mutex poisoned");

    // 1. Optimistic-concurrency guard.
    if let Some(expected) = &expect {
        if expected != &canonical.content_hash {
            return Response::Conflict {
                expected: expected.clone(),
                actual: canonical.content_hash.clone(),
            };
        }
    }

    // 2. Resolve the source. A read failure is a domain rejection (no install, prior retained).
    let (json, resolver) = match resolve_source(&state.resolve, source) {
        Ok(pair) => pair,
        Err(message) => {
            return Response::SwapReport(SwapReport {
                report: reuben_core::Report {
                    ok: false,
                    errors: vec![Diag {
                        node: None,
                        port: None,
                        message,
                    }],
                    warnings: Vec::new(),
                },
                content_hash: canonical.content_hash.clone(),
                diff: None,
            });
        }
    };

    // 3. Validate (load + plan). Retain-prior on any error.
    let report = validate(&json, &Registry::builtin(), &resolver);
    if !report.ok {
        return Response::SwapReport(SwapReport {
            report,
            content_hash: canonical.content_hash.clone(),
            diff: None,
        });
    }

    // The document validated: mint its canonical form for the new hash + serialized value.
    // `from_json` is load-only and just succeeded inside `validate`, so this cannot fail; a
    // defensive channel `Error` keeps the one-response invariant if it somehow does.
    let new_doc = match NormalizedDoc::from_json(
        &json,
        &Registry::builtin(),
        Some(&resolver as &dyn ResourceResolver),
    ) {
        Ok(doc) => doc,
        Err(e) => {
            return Response::Error {
                message: format!("normalize swapped document: {e}"),
            }
        }
    };
    let new_hash = content_hash(&new_doc);
    let new_value =
        serde_json::to_value(&*new_doc).expect("canonical instrument document serializes to JSON");
    let diff = restart_diff(&canonical.document, &new_value);

    // 4. Install onto the device (stop-the-world restart). Only reached post-validation, so a
    //    failure is a device/stream fault — surfaced as a channel Error, prior doc/hash intact.
    if let Err(message) = state.installer.restart(&json, resolver) {
        return Response::Error { message };
    }

    // 5. Commit.
    canonical.document = new_value;
    canonical.content_hash = new_hash.clone();
    Response::SwapReport(SwapReport {
        report,
        content_hash: new_hash,
        diff: Some(diff),
    })
}

/// Resolve a [`DocSource`] to its JSON text and a resolver anchored per ADR-0046 §8: a by-value
/// document reads inline JSON against the retained `base_dir`; a by-path document reads the file
/// and anchors at its own directory (mirroring `read_instrument`). Both apply the library-root
/// fallback when configured. A read/serialize failure is returned as a human message the caller
/// turns into a rejected swap.
fn resolve_source(
    resolve: &ResolveConfig,
    source: DocSource,
) -> Result<(String, FsResolver), String> {
    match source {
        DocSource::Document(value) => {
            let json = serde_json::to_string(&value)
                .map_err(|e| format!("serialize inline swap document: {e}"))?;
            let mut resolver = FsResolver::new(&resolve.base_dir);
            if let Some(root) = &resolve.root {
                resolver = resolver.with_root(root);
            }
            Ok((json, resolver))
        }
        DocSource::Path(path) => {
            let path = Path::new(&path);
            let json = std::fs::read_to_string(path)
                .map_err(|e| format!("read {}: {e}", path.display()))?;
            let mut resolver = FsResolver::for_instrument(path);
            if let Some(root) = &resolve.root {
                resolver = resolver.with_root(root);
            }
            Ok((json, resolver))
        }
    }
}

/// The M1 restart-swap [`DiffSummary`] (ADR-0046 §10): every node is cold, so `survived` is
/// always `0`. Node addresses present in **both** documents are `state_reset` (they exist but
/// their state was thrown away by the restart), new-only addresses are `added`, and old-only
/// addresses are `removed` — the whole-document re-emission accidents ADR-0048 §5 wants an
/// author to catch. M2 fills real survivor stats behind this unchanged shape.
fn restart_diff(old: &Value, new: &Value) -> DiffSummary {
    let old_addrs = node_addresses(old);
    let new_addrs = node_addresses(new);
    DiffSummary {
        survived: 0,
        state_reset: old_addrs.intersection(&new_addrs).cloned().collect(),
        added: new_addrs.difference(&old_addrs).cloned().collect(),
        removed: old_addrs.difference(&new_addrs).cloned().collect(),
    }
}

/// The set of top-level node addresses in a serialized instrument document — the keys the M1
/// diff compares. A [`BTreeSet`] so the diff lists come out sorted and deduped; a document with
/// no `nodes` array (or malformed entries) contributes none.
fn node_addresses(doc: &Value) -> BTreeSet<String> {
    doc.get("nodes")
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| n.get("address").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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

    /// A second valid instrument to swap to — a distinct graph (adds an explicit `/out` node and
    /// a different name) that both loads and plans, so a successful swap visibly changes
    /// `get_document` and the content hash.
    const SWAP_TARGET: &str = r#"{
        "format_version": 3,
        "instrument": "swapped-rig",
        "interface": { "outputs": { "main": { "from": "/out.audio" } } },
        "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": 330.0 } },
            { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/osc.audio" } } }
        ]
    }"#;

    /// A document that fails to load — an unknown operator type — so validation rejects it.
    const BAD_DOC: &str = r#"{"format_version":3,"instrument":"bad",
        "nodes":[{"type":"no_such_operator","address":"/x"}]}"#;

    /// A [`SwapInstaller`] that records each restart's document (so a test can assert the device
    /// side was — or was not — reached) and can be told to fail (a simulated post-validation
    /// device fault).
    #[derive(Default)]
    struct RecordingInstaller {
        calls: Mutex<Vec<String>>,
        fail: Option<String>,
    }

    impl SwapInstaller for RecordingInstaller {
        fn restart(&self, json: &str, _resolver: FsResolver) -> Result<(), String> {
            self.calls.lock().unwrap().push(json.to_string());
            match &self.fail {
                Some(message) => Err(message.clone()),
                None => Ok(()),
            }
        }
    }

    /// The base state to swap from (the minimal `doc()` rig) plus the recording installer and the
    /// base document's content hash.
    fn swap_fixture() -> (StructureState, Arc<RecordingInstaller>, String) {
        let base = doc();
        let base_hash = content_hash(&base);
        let installer = Arc::new(RecordingInstaller::default());
        let state =
            StructureState::from_doc(&base, Diagnostics::new()).with_installer(installer.clone());
        (state, installer, base_hash)
    }

    fn swap_by_value(target: &str, expect: Option<String>) -> Request {
        Request::Swap {
            source: DocSource::Document(serde_json::from_str(target).expect("target parses")),
            expect,
        }
    }

    fn target_hash() -> String {
        content_hash(
            &NormalizedDoc::from_json(SWAP_TARGET, &Registry::builtin(), None).expect("mint"),
        )
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
    fn swap_by_value_installs_validates_and_updates_the_document() {
        let (state, installer, base_hash) = swap_fixture();
        match dispatch(&state, &swap_by_value(SWAP_TARGET, None).to_ndjson()) {
            Response::SwapReport(report) => {
                assert!(report.report.ok, "a valid document installs: {report:?}");
                assert_eq!(
                    report.content_hash,
                    target_hash(),
                    "the installed hash is the new doc's"
                );
                let diff = report.diff.expect("a successful swap carries a diff");
                assert_eq!(diff.survived, 0, "M1 restart is all-cold (ADR-0046 §10)");
                // `/osc` exists in both docs → state_reset; `/out` is new → added.
                assert!(
                    diff.state_reset.contains(&"/osc".to_string()),
                    "diff: {diff:?}"
                );
                assert!(diff.added.contains(&"/out".to_string()), "diff: {diff:?}");
                assert!(diff.removed.is_empty(), "diff: {diff:?}");
            }
            other => panic!("expected SwapReport, got {other:?}"),
        }
        // The device restart was invoked exactly once, with the swapped document.
        assert_eq!(installer.calls.lock().unwrap().len(), 1);
        assert_ne!(
            target_hash(),
            base_hash,
            "the swap actually changed the document"
        );

        // get_document now returns the new doc + its hash.
        match dispatch(&state, &Request::GetDocument.to_ndjson()) {
            Response::Document {
                document,
                content_hash: hash,
            } => {
                assert_eq!(document["instrument"], serde_json::json!("swapped-rig"));
                assert_eq!(hash, target_hash());
            }
            other => panic!("expected Document, got {other:?}"),
        }
    }

    #[test]
    fn swap_by_path_reads_the_file_and_installs() {
        let (state, installer, _) = swap_fixture();
        let path =
            std::env::temp_dir().join(format!("reuben_swap_target_{}.json", std::process::id()));
        std::fs::write(&path, SWAP_TARGET).expect("write swap target");

        let req = Request::Swap {
            source: DocSource::Path(path.display().to_string()),
            expect: None,
        };
        match dispatch(&state, &req.to_ndjson()) {
            Response::SwapReport(report) => {
                assert!(report.report.ok, "a valid file installs: {report:?}")
            }
            other => panic!("expected SwapReport, got {other:?}"),
        }
        assert_eq!(installer.calls.lock().unwrap().len(), 1);
        match dispatch(&state, &Request::GetDocument.to_ndjson()) {
            Response::Document { document, .. } => {
                assert_eq!(document["instrument"], serde_json::json!("swapped-rig"))
            }
            other => panic!("expected Document, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn swap_of_a_bad_document_reports_errors_and_retains_prior() {
        let (state, installer, base_hash) = swap_fixture();
        match dispatch(&state, &swap_by_value(BAD_DOC, None).to_ndjson()) {
            Response::SwapReport(report) => {
                assert!(!report.report.ok, "a bad document does not install");
                assert!(
                    !report.report.errors.is_empty(),
                    "the failure names its cause"
                );
                assert_eq!(
                    report.content_hash, base_hash,
                    "the retained hash still names what keeps playing"
                );
                assert!(report.diff.is_none(), "a rejected swap has no diff");
            }
            other => panic!("expected SwapReport, got {other:?}"),
        }
        // Retain-prior: the device restart was never invoked, and get_document is unchanged.
        assert!(
            installer.calls.lock().unwrap().is_empty(),
            "a bad document must not restart audio"
        );
        match dispatch(&state, &Request::GetDocument.to_ndjson()) {
            Response::Document {
                content_hash: hash, ..
            } => assert_eq!(hash, base_hash),
            other => panic!("expected Document, got {other:?}"),
        }
    }

    #[test]
    fn swap_with_a_stale_expect_conflicts_and_does_not_restart() {
        let (state, installer, base_hash) = swap_fixture();
        let req = swap_by_value(SWAP_TARGET, Some("0badc0de0badc0de".to_string()));
        match dispatch(&state, &req.to_ndjson()) {
            Response::Conflict { expected, actual } => {
                assert_eq!(expected, "0badc0de0badc0de");
                assert_eq!(actual, base_hash, "conflict names the real installed hash");
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        assert!(
            installer.calls.lock().unwrap().is_empty(),
            "a conflict must not restart audio"
        );
    }

    #[test]
    fn swap_with_a_matching_expect_succeeds() {
        let (state, installer, base_hash) = swap_fixture();
        match dispatch(
            &state,
            &swap_by_value(SWAP_TARGET, Some(base_hash)).to_ndjson(),
        ) {
            Response::SwapReport(report) => assert!(report.report.ok),
            other => panic!("expected SwapReport, got {other:?}"),
        }
        assert_eq!(installer.calls.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_successful_swap_updates_the_hash_for_a_later_expect() {
        let (state, _installer, base_hash) = swap_fixture();
        dispatch(&state, &swap_by_value(SWAP_TARGET, None).to_ndjson());
        let new_hash = target_hash();
        assert_ne!(new_hash, base_hash);

        // The old hash is now stale — a swap guarded by it conflicts with the *new* installed hash.
        match dispatch(
            &state,
            &swap_by_value(SWAP_TARGET, Some(base_hash)).to_ndjson(),
        ) {
            Response::Conflict { actual, .. } => assert_eq!(actual, new_hash),
            other => panic!("a stale expect must conflict, got {other:?}"),
        }
    }

    #[test]
    fn a_device_fault_during_reopen_is_a_channel_error_with_prior_retained() {
        let base = doc();
        let base_hash = content_hash(&base);
        let installer = Arc::new(RecordingInstaller {
            calls: Mutex::new(Vec::new()),
            fail: Some("reopen audio after swap: no default output device".to_string()),
        });
        let state = StructureState::from_doc(&base, Diagnostics::new()).with_installer(installer);
        match dispatch(&state, &swap_by_value(SWAP_TARGET, None).to_ndjson()) {
            // Post-validation device fault → channel Error (ADR-0048 §3), not a SwapReport.
            Response::Error { message } => assert!(message.contains("no default output device")),
            other => panic!("a device fault must be a channel Error, got {other:?}"),
        }
        // The install failed after validation, so the prior doc/hash are left intact.
        match dispatch(&state, &Request::GetDocument.to_ndjson()) {
            Response::Document {
                content_hash: hash, ..
            } => assert_eq!(hash, base_hash),
            other => panic!("expected Document, got {other:?}"),
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
