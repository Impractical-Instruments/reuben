//! The structure channel: a loopback-TCP / NDJSON server (ADR-0046 §8).
//!
//! This is the engine-side half of the sidecar↔engine control channel ADR-0044 delegated to
//! ADR-0046: **TCP on `127.0.0.1`** (loopback-only — structure edits are more powerful than
//! control, unlike OSC's `0.0.0.0:9000`), carrying **newline-delimited JSON**, one
//! [`Response`] per [`Request`] in order. A thread in `reuben play` owns it; the client lives
//! in `reuben-mcp`. Zero new dependencies beyond std, cross-platform, netcat-debuggable.
//!
//! M2 (#323) flips the `swap` verb from M1's stop-the-world restart onto the
//! [`Coordinator`](reuben_core::coordinator::Coordinator)/mailbox path (ADR-0046 §10: *same
//! verb, machinery-only replacement*). The four verbs:
//! - [`Request::Ping`] → [`Response::Pong`] — liveness of the channel itself (ADR-0044 §2).
//! - [`Request::GetDocument`] → the Coordinator's canonical document + its
//!   [`content_hash`](reuben_core::content_hash) (ADR-0046 §7/§9). It changes only when a
//!   [`Request::Swap`] installs a new document.
//! - [`Request::GetDiagnostics`] → a [`DiagnosticsReport`] built from a live [`Snapshot`] of
//!   the [`Diagnostics`] `audio::start` owns (ADR-0038 §9 / ADR-0048 §6). **RT-safety:** the
//!   snapshot is [`Diagnostics::snapshot`]'s `Relaxed` atomic loads into an owned copy — the
//!   query thread never forces the audio callback to synchronize.
//! - [`Request::Swap`] → a **mailbox swap** (ADR-0046 §§1–7): [`Coordinator::swap_document`]
//!   validates + builds a whole new Engine off-thread, fills the install mailbox, and returns a
//!   real [`SwapReport`] with survivor/reset stats. The RT callback drains the mailbox and
//!   box-transplants the survivors gaplessly (ADR-0050's ramp) — **no stream teardown**; the
//!   streams are fixed at `play` start (ADR-0046 §6). This server thread then reclaims the
//!   retired Engine off-thread (ADR-0009), and publishes the freshly-validated device output map
//!   for the new engine through the injected [`RenderConfigPublisher`] seam. A swapped-in engine
//!   binding input channels no open stream provides **dark-degrades to silence** with a loud
//!   swap-report warning (ADR-0038 §7/§9), never an error or a crash.
//!
//! The Coordinator is single-writer (ADR-0046 §7): the structure server holds it behind one
//! [`Mutex`] so concurrent connections serialize, and the whole `expect`-compare → swap →
//! publish → reclaim runs as one critical section (a correct compare-and-swap, ADR-0046 §9).
//!
//! reuben-native stays **tokio-free** (ADR-0044 §3 fence): the server is a dedicated std
//! thread doing blocking line I/O, never an async runtime.

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use reuben_core::coordinator::{Coordinator, DiagnosticsReport, DocSource, Request, Response};
use reuben_core::{Diag, SwapReport};

use crate::diagnostics::{Diagnostics, Snapshot};

/// How long the accept loop sleeps between polls of its shutdown flag while idle. The listener
/// is non-blocking so the loop can observe [`StructureServer::shutdown`] without a client ever
/// connecting; this cadence is the shutdown latency, not a hot path (never the audio thread).
const ACCEPT_POLL: Duration = Duration::from_millis(50);

/// How long a connection handler's read blocks before waking to re-check its shutdown flag. A
/// [`shutdown`](StructureServer::shutdown) does **not** interrupt a `read` already blocked on a
/// socket on Windows (unlike Unix, where the accept thread's `shutdown(Shutdown::Both)` unblocks
/// the peer's `read_line`); a blocked recv there strands the handler until an OS-level connection
/// timeout (~2 min), so every connection torn down while a client is still attached pays that.
/// A read timeout makes the wait self-interrupting on every platform: the handler polls the flag
/// on each timeout instead of trusting a cross-thread wake. On a stream socket a timeout only
/// fires with *no* bytes available (a partial line would have returned immediately), so it never
/// splits a request — the framing the blocking read gave us is preserved. This cadence is the
/// per-connection shutdown latency, not a hot path (never the audio thread).
const READ_POLL: Duration = Duration::from_millis(250);

/// How long a swap waits for the RT callback to drain the install mailbox and post the retired
/// Engine back before giving up the off-thread reclaim (ADR-0046 §2 "engine isn't consuming
/// swaps; is audio running?"). With a live callback the retiree comes home in one master-gain
/// ramp (~20ms, ADR-0050); this bound only bites when audio has genuinely stopped — the swap has
/// already committed, so a timeout just defers the free to the next swap's opportunistic reclaim.
const SWAP_RECLAIM_TIMEOUT: Duration = Duration::from_millis(500);

/// How long a swap's bounded install/reclaim poll waits for the render callback to prove it is
/// running before concluding audio has *stopped* and bailing early (issue #373 note 2). Both the
/// map install ([`RenderConfigPublisher::publish`]) and the engine reclaim poll under the
/// Coordinator lock; when audio has genuinely stopped, waiting the full [`SWAP_RECLAIM_TIMEOUT`]
/// needlessly holds that lock (stalling `get_document` and the next swap for up to ~1s across both
/// polls). The [`SwapPollGate`] watches the render heartbeat: the instant the callback is seen to
/// advance it honors the full deadline (a live-but-slow device is never cut off), but if the
/// callback never ticks within this grace the poll gives up — well above any plausible callback
/// period (a 4096-frame block at 44.1 kHz is ~93ms) so a live callback is never mistaken for a
/// stopped one, yet far below the full deadline so the lock is released promptly.
const SWAP_LIVENESS_GRACE: Duration = Duration::from_millis(150);

/// Fast-bail gate for a swap's bounded install/reclaim poll (issue #373 note 2).
///
/// Both polls run under the Coordinator lock and, absent this gate, spin to a fixed ~500ms deadline
/// whenever the render side is not consuming — which starves `get_document` and the next swap when
/// audio has genuinely stopped. The gate distinguishes *stopped* from *slow* by watching the render
/// callback's heartbeat: once the callback is observed to advance even once, the poll honors the
/// full `hard` deadline (a live-but-slow device — a long ramp, a fat buffer — must never be cut
/// off); if the callback never ticks within [`SWAP_LIVENESS_GRACE`], the poll gives up early. A
/// headless publisher reports no heartbeat (`None`) and so always bails at the grace — there is no
/// render thread to consume, exactly the case the early-out is for.
///
/// This never shortens a *successful* poll: `reclaim`/`install` return the moment the retiree or
/// install slot is free, before the gate is ever consulted. It only bounds the *failure* wait.
pub(crate) struct SwapPollGate {
    /// The heartbeat sampled at poll start; `None` when the publisher drives no live render thread.
    baseline: Option<u64>,
    /// Set once the heartbeat is observed past `baseline` — proof the callback is live.
    seen_live: bool,
    /// Bail after this instant if the callback has not been seen live (the stopped-audio early-out).
    grace: Instant,
    /// The hard deadline honored once the callback is known live (the pre-existing generous bound).
    hard: Instant,
}

impl SwapPollGate {
    /// Start a gate for a poll bounded by `hard`, sampling the render heartbeat at `now`.
    pub(crate) fn start(heartbeat: Option<u64>, hard: Duration) -> Self {
        Self::start_at(heartbeat, Instant::now(), hard)
    }

    /// [`start`](Self::start) with an explicit clock, so the gate's logic is unit-testable without
    /// sleeping.
    pub(crate) fn start_at(heartbeat: Option<u64>, now: Instant, hard: Duration) -> Self {
        Self {
            baseline: heartbeat,
            seen_live: false,
            // Never let the grace outrun the hard deadline (a very short `hard` in a test).
            grace: now + SWAP_LIVENESS_GRACE.min(hard),
            hard: now + hard,
        }
    }

    /// Consult after each empty poll with the *current* heartbeat: `true` means give up.
    pub(crate) fn give_up(&mut self, heartbeat: Option<u64>) -> bool {
        self.give_up_at(heartbeat, Instant::now())
    }

    /// [`give_up`](Self::give_up) with an explicit clock (unit-test seam).
    pub(crate) fn give_up_at(&mut self, heartbeat: Option<u64>, now: Instant) -> bool {
        if let (Some(base), Some(cur)) = (self.baseline, heartbeat) {
            if cur != base {
                self.seen_live = true;
            }
        }
        now >= self.hard || (!self.seen_live && now >= self.grace)
    }
}

/// Publish the render-side config for a freshly-installed engine and report any dark-degrade
/// warnings (ADR-0046 §6, ADR-0038 §7/§9) — the **native device seam** of the M2 swap.
///
/// After [`Coordinator::swap_document`] commits, this rebuilds the device **output map**
/// off-thread against the *retained* device channel count (streams are fixed at `play` start,
/// ADR-0046 §6) and ships it across the render mailbox to the RT callback, so the callback
/// installs Engine + map together. It also decides the **input dark-degrade**: a swapped-in
/// engine that binds input channels no open stream provides degrades to silence, and this returns
/// the loud swap-report warning (§9 know-and-say) — not an error, not a crash.
///
/// `play` wires the production implementation (the native map mailbox, in `audio.rs`); a headless
/// test uses [`HeadlessRenderConfig`], which builds no map (there is no device) but still computes
/// the dark-degrade warning from the retained geometry, so the swap **logic** — including the
/// warning — is exercised with no cpal device (ADR-0053 §4).
///
/// `Send + Sync` because a connection-handler thread calls it (under the Coordinator lock).
pub trait RenderConfigPublisher: Send + Sync {
    /// Publish the render config for the just-installed engine with `logical` output channels and
    /// `input_channels` input channels, and return any dark-degrade warnings to fold into the
    /// [`SwapReport`].
    fn publish(&self, logical: usize, input_channels: usize) -> Vec<Diag>;

    /// A monotonic count of render callbacks observed so far, or `None` if this publisher drives no
    /// live render thread (the headless case). The engine reclaim's [`SwapPollGate`] samples it to
    /// tell a running device from a stopped one and bail early instead of holding the Coordinator
    /// lock to the full [`SWAP_RECLAIM_TIMEOUT`] (issue #373 note 2). Default `None`: a publisher
    /// with no device is treated as not consuming, so the reclaim bails at the grace window.
    fn render_heartbeat(&self) -> Option<u64> {
        None
    }
}

/// The default [`RenderConfigPublisher`] for a headless [`StructureState`] (ADR-0053 §4): no
/// device, so no output map is built or shipped, but the **input dark-degrade** warning is still
/// computed from the retained input-stream geometry — the device-independent half the integration
/// and unit tests drive. `opened_input_channels` is how many logical input channels the input
/// stream that opened at `play` start provides (`0` = output-only, no input stream). Production
/// overrides this with the real native map publisher.
pub struct HeadlessRenderConfig {
    /// Logical input channels the play-start input stream provides; `0` for an output-only stream.
    pub opened_input_channels: usize,
}

impl RenderConfigPublisher for HeadlessRenderConfig {
    fn publish(&self, _logical: usize, input_channels: usize) -> Vec<Diag> {
        dark_degrade_warning(input_channels, self.opened_input_channels)
    }
}

/// The input dark-degrade warning (ADR-0038 §7/§9), shared by the headless and production
/// publishers so the two can't drift. A swapped-in engine wanting `input_channels` logical input
/// channels while the open input stream provides `opened_input_channels`: any shortfall (the
/// output-only-stream case is `opened == 0`) means some bound input pipes have no live stream and
/// **dark-degrade to silence** — a loud warning, never an error (the engine stays silent-but-alive;
/// a device-topology change needs a `play` restart, ADR-0046 §6). Matched geometry is silent.
pub(crate) fn dark_degrade_warning(
    input_channels: usize,
    opened_input_channels: usize,
) -> Vec<Diag> {
    if input_channels > opened_input_channels {
        vec![Diag {
            node: None,
            port: None,
            message: format!(
                "swapped-in instrument binds {input_channels} input channel(s) but the open input \
                 stream provides {opened_input_channels} (fixed at `play` start, ADR-0046 §6); the \
                 unmatched input pipes dark-degrade to silence (ADR-0038 §7). The engine stays \
                 alive and silent; restart `play` with a matching input-binding instrument to \
                 capture live input."
            ),
        }]
    } else {
        Vec::new()
    }
}

/// Everything the structure server answers with, cheap to clone (`Arc`-backed) so every
/// connection-handler thread holds its own handle.
///
/// The [`Coordinator`] behind one [`Mutex`] is the single writer of graph structure (ADR-0046
/// §7): it owns the canonical document + hash (`swap` advances them, `get_document`/`expect` read
/// them) and the install mailbox. The `render_config` seam publishes the device output map + the
/// dark-degrade warning after each swap; `diagnostics` is the live counter surface the callback
/// feeds (fixed at `play` start — M2 never reopens streams, so it never re-points).
#[derive(Clone)]
pub struct StructureState {
    coordinator: Arc<Mutex<Coordinator>>,
    diagnostics: Arc<Diagnostics>,
    render_config: Arc<dyn RenderConfigPublisher>,
}

impl StructureState {
    /// Wrap a [`Coordinator`] `play` (or a test) built with [`Coordinator::install_initial`],
    /// alongside the live [`Diagnostics`] surface. Built with the headless [`HeadlessRenderConfig`]
    /// (output-only: any input-binding swap dark-degrades) — production wires the real native map
    /// publisher with [`with_render_config`](Self::with_render_config).
    pub fn from_coordinator(coordinator: Coordinator, diagnostics: Arc<Diagnostics>) -> Self {
        Self {
            coordinator: Arc::new(Mutex::new(coordinator)),
            diagnostics,
            render_config: Arc::new(HeadlessRenderConfig {
                opened_input_channels: 0,
            }),
        }
    }

    /// Wire the production render-config seam (`play`'s native map publisher). Without this a swap
    /// updates the installed document + reclaims the retiree but ships no output map and reports
    /// the headless dark-degrade warning (the test seam).
    pub fn with_render_config(mut self, render_config: Arc<dyn RenderConfigPublisher>) -> Self {
        self.render_config = render_config;
        self
    }

    /// The live diagnostics surface, so `play` can flush a final exit-time snapshot (ADR-0038 §9).
    pub fn diagnostics(&self) -> Arc<Diagnostics> {
        Arc::clone(&self.diagnostics)
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
            // The Coordinator owns the canonical document (ADR-0046 §7); serialize it + its hash
            // under the lock so `get_document` never sees a half-installed pair.
            let coordinator = state
                .coordinator
                .lock()
                .expect("coordinator mutex poisoned");
            let document = serde_json::to_value(&**coordinator.document())
                .expect("canonical instrument document serializes to JSON");
            Response::Document {
                document,
                content_hash: coordinator.installed_hash(),
            }
        }
        // RT-safe read: `Relaxed` loads into an owned copy off this (non-audio) thread.
        Ok(Request::GetDiagnostics) => {
            Response::Diagnostics(diagnostics_report(&state.diagnostics.snapshot()))
        }
        Ok(Request::Swap { source, expect }) => handle_swap(state, source, expect),
        Err(e) => Response::Error {
            message: format!("unreadable request: {e}"),
        },
    }
}

/// The M2 mailbox-swap install path (ADR-0046 §§1–10), device-free up to the
/// [`RenderConfigPublisher`] call. Everything runs under the Coordinator lock so the
/// `expect`-compare and the swap are one atomic critical section (ADR-0046 §9's compare-and-swap)
/// — concurrent swaps from multiple connections serialize, and neither `get_document` nor another
/// swap sees a half-installed document. In order:
///
/// 1. **Resolve** the [`DocSource`] to its JSON text — inline JSON re-serialized, or a file read
///    (ADR-0046 §8). Resources resolve through the Coordinator's own resolver (anchored at `play`
///    start). A read failure is a rejected [`SwapReport`] (no install, prior retained), not a
///    channel `Error`.
/// 2. **Arbitration** (ADR-0046 §9): a stale `expect` rejects with the real installed hash as
///    [`Response::Conflict`] and does **not** swap. Absent `expect` is last-write-wins. Done here
///    (not inside `swap_document`) so the wire keeps M1's distinct `Conflict` response shape.
/// 3. **Swap**: [`Coordinator::swap_document`] validates + builds a whole new Engine off-thread,
///    fills the install mailbox, and returns the real [`SwapReport`] (survivor/reset stats). A
///    load/plan error aborts with `ok: false` and the prior hash — the old engine keeps playing
///    (retain-prior, ADR-0046 §10). The RT callback installs it gaplessly at the next ramp.
/// 4. **Publish** the render config: rebuild the device output map off-thread for the new engine's
///    geometry and ship it across the render mailbox; fold any input dark-degrade warning (ADR-0038
///    §7/§9) into the report.
/// 5. **Reclaim** the retired Engine off-thread (ADR-0009 deferred free), clearing the mailbox for
///    the next swap.
fn handle_swap(state: &StructureState, source: DocSource, expect: Option<String>) -> Response {
    // 1. Resolve the source to JSON text. A read failure is a domain rejection (no install).
    let json = match resolve_source(source) {
        Ok(json) => json,
        Err(message) => return rejected_swap(&state.coordinator, message),
    };

    let mut coordinator = state
        .coordinator
        .lock()
        .expect("coordinator mutex poisoned");

    // 2. Optimistic-concurrency guard (ADR-0046 §9): a stale expect is a Conflict, no swap.
    if let Some(expected) = &expect {
        let actual = coordinator.installed_hash();
        if expected != &actual {
            return Response::Conflict {
                expected: expected.clone(),
                actual,
            };
        }
    }

    // 3. Swap via the mailbox. `expect` is already honored above, so pass `None`.
    let mut report = coordinator.swap_document(&json, None);
    if report.report.ok {
        // 4. Publish the new engine's device output map + fold the dark-degrade warning — BEFORE the
        //    engine reclaim (B1). `publish` fills the output-map mailbox (never dropping the map),
        //    so the map is in flight *before* the callback installs the new engine; the callback
        //    then promotes it the moment the engine reaches the new width, keeping the two mailboxes
        //    in lockstep with no desync window. Publishing after the reclaim would leave a block
        //    where the engine has widened but its map has not arrived yet.
        let logical = coordinator.installed_channels();
        let input_channels = coordinator.installed_input_channels();
        report
            .report
            .warnings
            .extend(state.render_config.publish(logical, input_channels));

        // 5. Reclaim the retired Engine off-thread (this structure thread, never the callback). This
        //    also proves the callback is consuming — the retiree comes home at the ramp
        //    zero-crossing — so `publish`'s bounded install poll above can never wedge. The render
        //    heartbeat lets the reclaim bail early if audio has genuinely stopped rather than hold
        //    this lock to the full deadline (issue #373 note 2).
        reclaim_retired_engine(&mut coordinator, || state.render_config.render_heartbeat());
    }
    Response::SwapReport(report)
}

/// A rejected swap that never reached [`Coordinator::swap_document`] (a source read failure):
/// `ok: false`, the message, no diff, and the still-installed hash — the report names what keeps
/// playing (ADR-0046 §10 retain-prior).
fn rejected_swap(coordinator: &Arc<Mutex<Coordinator>>, message: String) -> Response {
    let content_hash = coordinator
        .lock()
        .expect("coordinator mutex poisoned")
        .installed_hash();
    Response::SwapReport(SwapReport {
        report: reuben_core::Report {
            ok: false,
            errors: vec![Diag {
                node: None,
                port: None,
                message,
            }],
            warnings: Vec::new(),
        },
        content_hash,
        diff: None,
    })
}

/// Reclaim the retired [`InstallBundle`](reuben_core::coordinator::InstallBundle) the RT callback
/// posted back and **drop it here, off the audio thread** — ADR-0009's deferred free. Polls the
/// retire slot with a 1ms back-off (the caller supplies the clock; core is OS-free), bounded by the
/// [`SwapPollGate`]: at most [`SWAP_RECLAIM_TIMEOUT`] while the callback is proven live, but only
/// [`SWAP_LIVENESS_GRACE`] once `heartbeat` shows audio has stopped ticking (issue #373 note 2). A
/// timeout is not fatal: the swap already committed, so it just leaves the retiree in flight for the
/// next swap's opportunistic reclaim (ADR-0046 §2) — the "audio isn't consuming swaps" case.
fn reclaim_retired_engine(coordinator: &mut Coordinator, heartbeat: impl Fn() -> Option<u64>) {
    let mut gate = SwapPollGate::start(heartbeat(), SWAP_RECLAIM_TIMEOUT);
    match coordinator.reclaim(|| {
        std::thread::sleep(Duration::from_millis(1));
        gate.give_up(heartbeat())
    }) {
        // Dropping the reclaimed bundle frees the retired Engine here, off the render thread.
        Ok(retiree) => drop(retiree),
        Err(_) => { /* audio not consuming yet; the next swap or shutdown reclaims it */ }
    }
}

/// Resolve a [`DocSource`] to its JSON text (ADR-0046 §8): inline JSON re-serialized to a string,
/// or a file read. Resource paths inside the document resolve through **the Coordinator's own
/// resolver**, anchored once at `play` start against the initial instrument's directory + the
/// library root.
///
/// **Behavior change from M1 (sanctioned by ADR-0046 §7).** M1 re-anchored a by-*path* swap at the
/// swapped file's own directory (`FsResolver::for_instrument`). ADR-0046 §7 gives the Coordinator a
/// single owned resolver ("owns the Registry handle, the resolver, …"), and M2's `swap_document`
/// uses exactly that one resolver — so M2 does **not** re-anchor per swap source. By-*value* swaps
/// (the MCP primary flow) are unchanged: their resources always resolved against the play-start
/// anchor. A by-*path* swap's *relative* resources now resolve against that anchor + the library
/// root rather than the file's own directory; an unresolvable one dark-degrades to a `LoadWarning`
/// (or, if structurally required, a clean `ok:false` reject), never a crash. A read/serialize
/// failure here is a human message the caller turns into a rejected swap.
fn resolve_source(source: DocSource) -> Result<String, String> {
    match source {
        DocSource::Document(value) => serde_json::to_string(&value)
            .map_err(|e| format!("serialize inline swap document: {e}")),
        DocSource::Path(path) => {
            std::fs::read_to_string(&path).map_err(|e| format!("read {path}: {e}", path = path))
        }
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

/// Wake a handler's blocked read (a socket `shutdown` on the clone unblocks its `read_line`) and
/// join it. Best-effort and idempotent: waking an already-finished handler is a harmless no-op and
/// joining it returns at once, so this serves both the mid-run reap (already finished) and the
/// shutdown drain (still live) — one shape, so the two sites can't drift.
fn wake_and_join(handle: JoinHandle<()>, wake: Option<TcpStream>) {
    if let Some(wake) = wake {
        let _ = wake.shutdown(Shutdown::Both);
    }
    let _ = handle.join();
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

        // Reap completed handlers during normal operation so this vector stays bounded.
        let mut i = 0;
        while i < handlers.len() {
            if handlers[i].0.is_finished() {
                let (handle, wake) = handlers.remove(i);
                wake_and_join(handle, wake);
            } else {
                i += 1;
            }
        }
    }
    for (handle, wake) in handlers {
        wake_and_join(handle, wake);
    }
}

/// Serve one connection: read one [`Request`] per line, write one [`Response`] per line, in
/// order (ADR-0046 §8), until the client closes or shutdown wakes the blocked read. Blocking
/// reads keep the framing exact; a blank line is framing noise, not a request, so it draws no
/// response.
fn handle_connection(stream: TcpStream, state: StructureState, shutdown: Arc<AtomicBool>) {
    // Time-bound each read so the handler wakes to observe `shutdown` itself, rather than
    // depending on the accept thread's `shutdown(Shutdown::Both)` to unblock it — a wake that
    // does not reach a blocked recv on Windows (see [`READ_POLL`]). A timeout on a stream socket
    // fires only when no bytes are waiting, so it never truncates a request mid-line; a request
    // that spans reads accumulates in `line` across the loop until its terminating newline.
    let _ = stream.set_read_timeout(Some(READ_POLL));
    let mut writer = match stream.try_clone() {
        Ok(w) => w,
        // Can't split the socket into independent read/write halves; nothing to serve on.
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        match reader.read_line(&mut line) {
            // EOF: client closed, or a shutdown `Shutdown::Both` woke us.
            Ok(0) => break,
            // A complete line (`read_line` returns on the newline). Dispatch, then reset the buffer.
            Ok(_) => {
                if shutdown.load(Ordering::SeqCst) {
                    break;
                }
                if !line.trim().is_empty() {
                    let response = dispatch(&state, &line);
                    if writer.write_all(response.to_ndjson().as_bytes()).is_err() {
                        break;
                    }
                    if writer.flush().is_err() {
                        break;
                    }
                }
                line.clear();
            }
            // The read timeout fired with no full line yet: loop to re-check `shutdown`, keeping
            // any partial bytes already read in `line` (`read_line` leaves them on error) so the
            // next read resumes the same request. This is the poll that makes shutdown prompt.
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            // A real read error ends the connection.
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reuben_core::coordinator::{RenderSide, RenderSlot};
    use reuben_core::resources::MemoryResolver;
    use reuben_core::{AudioConfig, Registry};
    use std::sync::atomic::AtomicBool;

    fn cfg() -> AudioConfig {
        AudioConfig::new(48_000.0, 128)
    }

    /// A minimal output-only rig to swap *from*: one oscillator through a master output.
    const BASE_DOC: &str = r#"{"format_version":3,"instrument":"t",
        "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

    /// A held envelope whose CV is the master output (rings at sustain) — a swap to the identical
    /// document keeps `/env` + `/out` survivors, so the real diff carries `survived: 2` (impossible
    /// under M1's all-cold restart), the load-bearing proof the mailbox migration ran.
    fn envelope_doc(env_addr: &str) -> String {
        format!(
            r#"{{ "format_version": 3, "instrument": "eg",
                 "interface": {{ "outputs": {{ "out": {{ "from": "/out.audio" }} }} }},
                 "nodes": [
                   {{ "type": "envelope", "address": "{env_addr}",
                      "inputs": {{ "gate": 1.0, "attack": 0.5, "decay": 0.01,
                                   "sustain": 0.8, "release": 0.5 }} }},
                   {{ "type": "output", "address": "/out",
                      "inputs": {{ "audio": {{ "from": "{env_addr}.cv" }} }} }} ] }}"#
        )
    }

    /// A minimal instrument that binds logical input channel 0 (ADR-0038 §3): the swapped-in engine
    /// wants live input, which an output-only stream can't provide — the dark-degrade case.
    const MIC_PASSTHRU: &str = r#"{ "format_version": 3, "instrument": "mic-passthru",
        "interface": {
            "inputs": { "mic": { "type": "f32_buffer", "channel": 0 } },
            "outputs": { "out": { "from": "/mic" } } },
        "nodes": [] }"#;

    /// A document that fails to load — an unknown operator type — so validation rejects it.
    const BAD_DOC: &str = r#"{"format_version":3,"instrument":"bad",
        "nodes":[{"type":"no_such_operator","address":"/x"}]}"#;

    /// A background "fake audio callback" (ADR-0053 §4): it owns the [`RenderSlot`] the real cpal
    /// callback would and drives it in a loop, draining the install mailbox, running the master-gain
    /// ramp, box-transplanting survivors, and posting retirees — so a Coordinator-driven swap
    /// installs **via the mailbox** and its `reclaim` completes, all with no audio device. Rendering
    /// the *logical* master directly (no device output map — that is the human ritual's half) is
    /// enough to make the swap real end-to-end.
    struct FakeCallback {
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl FakeCallback {
        fn spawn(side: RenderSide) -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let stop_thread = Arc::clone(&stop);
            let handle = std::thread::spawn(move || {
                let mut slot = RenderSlot::new(side);
                let mut buf = vec![0.0f32; 128 * slot.channels().max(1)];
                while !stop_thread.load(Ordering::SeqCst) {
                    let ch = slot.channels().max(1);
                    if buf.len() != 128 * ch {
                        buf.resize(128 * ch, 0.0);
                    }
                    slot.fill(&mut buf);
                    // Pace the loop like a device would; fast enough that a swap's ramp completes in
                    // a few ms, slow enough not to spin a core.
                    std::thread::sleep(Duration::from_millis(1));
                }
                // The slot (and its Engine + mailbox) drops here, off any RT thread.
            });
            Self {
                stop,
                handle: Some(handle),
            }
        }

        fn stop(mut self) {
            self.stop.store(true, Ordering::SeqCst);
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// A Coordinator-backed [`StructureState`] with a live [`FakeCallback`] draining its mailbox,
    /// plus the base document's content hash. `opened_input_channels` sets the headless render
    /// config's input-stream geometry (`0` = output-only, so an input-binding swap dark-degrades).
    fn swap_fixture(
        base: &str,
        opened_input_channels: usize,
    ) -> (StructureState, FakeCallback, String) {
        let (coordinator, side, _warnings) = Coordinator::install_initial(
            base,
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let base_hash = coordinator.installed_hash();
        let state = StructureState::from_coordinator(coordinator, Diagnostics::new())
            .with_render_config(Arc::new(HeadlessRenderConfig {
                opened_input_channels,
            }));
        (state, FakeCallback::spawn(side), base_hash)
    }

    fn swap_by_value(target: &str, expect: Option<String>) -> Request {
        Request::Swap {
            source: DocSource::Document(serde_json::from_str(target).expect("target parses")),
            expect,
        }
    }

    #[test]
    fn diagnostics_report_maps_every_counter_field_for_field() {
        // Distinct values per counter so a mis-wire (mapping overruns to underruns, say) is
        // caught — equal values would let a swapped pair pass.
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
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        assert_eq!(dispatch(&state, &Request::Ping.to_ndjson()), Response::Pong);
        cb.stop();
    }

    #[test]
    fn get_document_returns_the_coordinators_doc_and_hash() {
        let (state, cb, base_hash) = swap_fixture(BASE_DOC, 0);
        match dispatch(&state, &Request::GetDocument.to_ndjson()) {
            Response::Document {
                document,
                content_hash: hash,
            } => {
                assert_eq!(document["instrument"], serde_json::json!("t"));
                assert_eq!(hash, base_hash);
            }
            other => panic!("expected Document, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn get_diagnostics_reads_the_live_counters() {
        let (coordinator, side, _w) = Coordinator::install_initial(
            BASE_DOC,
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("install");
        let diagnostics = Diagnostics::new();
        let state = StructureState::from_coordinator(coordinator, Arc::clone(&diagnostics));
        let cb = FakeCallback::spawn(side);
        // Fresh: zeroed.
        assert_eq!(
            dispatch(&state, &Request::GetDiagnostics.to_ndjson()),
            Response::Diagnostics(DiagnosticsReport::default())
        );
        // A later bump is visible — the live Arc is read, not a frozen copy.
        diagnostics.record_output_xrun();
        assert_eq!(
            dispatch(&state, &Request::GetDiagnostics.to_ndjson()),
            Response::Diagnostics(DiagnosticsReport {
                output_xruns: 1,
                ..DiagnosticsReport::default()
            })
        );
        cb.stop();
    }

    #[test]
    fn swap_installs_via_the_mailbox_with_real_survivor_stats() {
        // The heart of M2 (ADR-0046 §§5,10): a swap to the identical envelope document keeps both
        // nodes survivors, so the real migration diff carries `survived: 2` — impossible under M1's
        // all-cold restart (which hard-codes `survived: 0`). The swap installed via the mailbox (the
        // FakeCallback drained it; `reclaim` completing is the proof), no stream teardown involved.
        let base = envelope_doc("/env");
        let (state, cb, base_hash) = swap_fixture(&base, 0);

        match dispatch(&state, &swap_by_value(&base, None).to_ndjson()) {
            Response::SwapReport(report) => {
                assert!(report.report.ok, "a valid document installs: {report:?}");
                let diff = report.diff.expect("a successful swap carries a diff");
                assert_eq!(
                    diff.survived, 2,
                    "both nodes survive an identical-document swap (real migration): {diff:?}"
                );
                // Same document -> same hash; the report names what is now playing.
                assert_eq!(report.content_hash, base_hash);
            }
            other => panic!("expected SwapReport, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn swap_to_a_renamed_node_resets_it_and_reports_a_smaller_survivor_count() {
        // The reset half: renaming the envelope makes it a remove+add, so only `/out` survives —
        // `survived: 1`, with `/env` removed and `/eg` added. Real survivor semantics from the
        // manifest diff, not M1's blanket zero.
        let (state, cb, _base_hash) = swap_fixture(&envelope_doc("/env"), 0);
        match dispatch(
            &state,
            &swap_by_value(&envelope_doc("/eg"), None).to_ndjson(),
        ) {
            Response::SwapReport(report) => {
                assert!(report.report.ok, "{report:?}");
                let diff = report.diff.expect("diff");
                assert_eq!(diff.survived, 1, "only /out survives a rename: {diff:?}");
                assert!(diff.removed.contains(&"/env".to_string()), "{diff:?}");
                assert!(diff.added.contains(&"/eg".to_string()), "{diff:?}");
            }
            other => panic!("expected SwapReport, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn back_to_back_swaps_both_install_proving_off_thread_reclaim() {
        // One swap in flight (ADR-0046 §2): a second swap can only install once the first's retiree
        // has come home and been reclaimed. Both succeeding is the behavioral proof the mailbox +
        // off-thread reclaim cycle actually turned over — the M2 mechanism, not a restart.
        let (state, cb, _) = swap_fixture(&envelope_doc("/env"), 0);
        for target in [
            envelope_doc("/env"),
            envelope_doc("/eg"),
            envelope_doc("/env"),
        ] {
            match dispatch(&state, &swap_by_value(&target, None).to_ndjson()) {
                Response::SwapReport(report) => assert!(report.report.ok, "{report:?}"),
                other => panic!("expected SwapReport, got {other:?}"),
            }
        }
        cb.stop();
    }

    #[test]
    fn input_binding_swap_onto_output_only_stream_dark_degrades_with_a_warning() {
        // ADR-0038 §7/§9: the initial rig is output-only (`opened_input_channels: 0`). A swap to an
        // instrument that binds an input channel installs and stays silent-but-alive — a loud
        // swap-report WARNING, never an error or a crash.
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        match dispatch(&state, &swap_by_value(MIC_PASSTHRU, None).to_ndjson()) {
            Response::SwapReport(report) => {
                assert!(
                    report.report.ok,
                    "dark-degrade is not an error — the engine stays alive: {report:?}"
                );
                assert!(
                    report
                        .report
                        .warnings
                        .iter()
                        .any(|w| w.message.contains("dark-degrade")),
                    "the swap report carries the loud dark-degrade warning: {report:?}"
                );
            }
            other => panic!("expected SwapReport, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn swap_of_a_bad_document_reports_errors_and_retains_prior() {
        let (state, cb, base_hash) = swap_fixture(BASE_DOC, 0);
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
        // get_document is unchanged.
        match dispatch(&state, &Request::GetDocument.to_ndjson()) {
            Response::Document {
                content_hash: hash, ..
            } => assert_eq!(hash, base_hash),
            other => panic!("expected Document, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn swap_with_a_stale_expect_conflicts_and_does_not_install() {
        let (state, cb, base_hash) = swap_fixture(BASE_DOC, 0);
        let req = swap_by_value(&envelope_doc("/env"), Some("0badc0de0badc0de".to_string()));
        match dispatch(&state, &req.to_ndjson()) {
            Response::Conflict { expected, actual } => {
                assert_eq!(expected, "0badc0de0badc0de");
                assert_eq!(actual, base_hash, "conflict names the real installed hash");
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
        // The installed document is unchanged.
        match dispatch(&state, &Request::GetDocument.to_ndjson()) {
            Response::Document {
                content_hash: hash, ..
            } => assert_eq!(hash, base_hash),
            other => panic!("expected Document, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn swap_with_a_matching_expect_succeeds() {
        let (state, cb, base_hash) = swap_fixture(BASE_DOC, 0);
        match dispatch(
            &state,
            &swap_by_value(&envelope_doc("/env"), Some(base_hash)).to_ndjson(),
        ) {
            Response::SwapReport(report) => assert!(report.report.ok, "{report:?}"),
            other => panic!("expected SwapReport, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn an_unreadable_line_is_a_channel_error() {
        let (state, cb, _) = swap_fixture(BASE_DOC, 0);
        match dispatch(&state, "{not json}\n") {
            Response::Error { message } => assert!(message.contains("unreadable request")),
            other => panic!("a malformed request must return Error, got {other:?}"),
        }
        cb.stop();
    }

    #[test]
    fn dark_degrade_warning_fires_only_on_unmatched_input_geometry() {
        // The pure rule (ADR-0038 §7): no warning when the engine needs no input or the stream
        // matches; a warning on any shortfall (output-only stream, or a topology change).
        assert!(
            dark_degrade_warning(0, 0).is_empty(),
            "no input, no warning"
        );
        assert!(
            dark_degrade_warning(2, 2).is_empty(),
            "matched geometry is silent"
        );
        assert!(
            !dark_degrade_warning(1, 0).is_empty(),
            "input onto output-only stream warns"
        );
        assert!(
            !dark_degrade_warning(3, 2).is_empty(),
            "a wider input than the stream warns"
        );
        assert!(
            dark_degrade_warning(1, 2).is_empty(),
            "a stream wider than the input is a surplus, not a shortfall — silent"
        );
    }

    #[test]
    fn swap_poll_gate_bails_at_grace_only_when_the_callback_is_not_live() {
        // Deterministic clock (fixed base + offsets) so the gate's decision is tested without
        // sleeping — issue #373 note 2's "stopped vs slow" distinction.
        let t0 = Instant::now();
        let hard = Duration::from_millis(500);

        // Stopped audio: the heartbeat never advances, so bail once past the grace (not the hard
        // deadline).
        let mut dead = SwapPollGate::start_at(Some(7), t0, hard);
        assert!(
            !dead.give_up_at(Some(7), t0 + Duration::from_millis(10)),
            "before the grace: keep polling"
        );
        assert!(
            dead.give_up_at(Some(7), t0 + SWAP_LIVENESS_GRACE + Duration::from_millis(1)),
            "past the grace with no heartbeat tick: give up"
        );

        // Live-but-slow: one observed tick pins the poll to the full hard deadline, past the grace.
        let mut live = SwapPollGate::start_at(Some(7), t0, hard);
        assert!(
            !live.give_up_at(
                Some(8),
                t0 + SWAP_LIVENESS_GRACE + Duration::from_millis(50)
            ),
            "seen live: the grace no longer applies, keep polling"
        );
        assert!(
            live.give_up_at(Some(9), t0 + hard),
            "a live poll is still bounded by the hard deadline"
        );

        // Headless (no heartbeat at all): always bails at the grace — nothing consumes.
        let mut headless = SwapPollGate::start_at(None, t0, hard);
        assert!(
            !headless.give_up_at(None, t0 + Duration::from_millis(10)),
            "before the grace: keep polling"
        );
        assert!(
            headless.give_up_at(None, t0 + SWAP_LIVENESS_GRACE + Duration::from_millis(1)),
            "past the grace with no heartbeat: give up"
        );
    }

    #[test]
    fn swap_reclaim_bails_fast_when_no_render_side_consumes() {
        // With nothing draining the mailbox (audio has stopped), the engine reclaim would otherwise
        // spin the full SWAP_RECLAIM_TIMEOUT under the Coordinator lock, stalling get_document and
        // the next swap (issue #373 note 2). No FakeCallback here — the render side is dropped, so
        // the retiree never comes home — so the liveness gate must bail at the grace instead.
        let (coordinator, _side, _w) = Coordinator::install_initial(
            BASE_DOC,
            Registry::builtin(),
            Box::new(MemoryResolver::new()),
            cfg(),
        )
        .expect("initial install");
        let state = StructureState::from_coordinator(coordinator, Diagnostics::new())
            .with_render_config(Arc::new(HeadlessRenderConfig {
                opened_input_channels: 0,
            }));

        let start = Instant::now();
        let resp = dispatch(
            &state,
            &swap_by_value(&envelope_doc("/env"), None).to_ndjson(),
        );
        let elapsed = start.elapsed();

        assert!(
            matches!(resp, Response::SwapReport(ref r) if r.report.ok),
            "the swap still commits and returns its report: {resp:?}"
        );
        assert!(
            elapsed < SWAP_RECLAIM_TIMEOUT,
            "reclaim bailed at the ~{SWAP_LIVENESS_GRACE:?} liveness grace, not the full \
             {SWAP_RECLAIM_TIMEOUT:?} (took {elapsed:?})"
        );
    }
}
