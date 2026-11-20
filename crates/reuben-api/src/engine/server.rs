//! The serving side of the structure channel: what each verb *means*, over the seam a host fills.
//!
//! One [`Response`] per [`Request`], in order. There is no socket here and no thread — a host owns
//! the listener, the framing loop and the shutdown; this is the part that would otherwise be
//! rewritten per host, and rewritten differently.
//!
//! The verbs:
//! - [`Request::Ping`] → [`Response::Pong`] — liveness of the channel itself.
//! - [`Request::GetDocument`] → the Coordinator's canonical document + its content hash. It changes
//!   only when a [`Request::Swap`] installs a new one.
//! - [`Request::GetDiagnostics`] → the host's counters, snapshotted off-thread.
//! - [`Request::GetMeters`] → the host's levels at the device, read the same way.
//! - [`Request::SetProbes`] → which nodes `GetMeters` also reports, chosen through the host.
//! - [`Request::Swap`] → a **mailbox swap**: the Coordinator validates and builds a whole new
//!   Engine off-thread, fills the install mailbox, and returns a real report with survivor/reset
//!   stats. The `expect` guard is applied here, inside the Coordinator lock, so the compare and the
//!   swap are one critical section.
//! - [`Request::Send`] → control traffic, handed to the host's ingress as **one** batch. The one
//!   verb that does not touch the Coordinator: control is not structure.
//! - [`Request::Call`] → one non-engine roster verb against the engine's store. A read answers
//!   directly. An edit waits in memory while the whole open instrument loads and plans with it;
//!   only then is it written and the new Plan swapped in, and otherwise nothing is written and the
//!   answer says why.
//! - [`Request::Subscribe`] is not a request [`dispatch`] answers: it turns a connection into a
//!   stream of [`Event`]s, which only the host's connection can carry. [`StructureState::subscribe`]
//!   is what that connection streams from, and what [`InProcess::subscribe`] hands back.
//!
//! Each install that goes live publishes one [`Event`], under the Coordinator lock, so subscribers
//! hear them in install order.
//!
//! The Coordinator is single-writer, so it sits behind one [`Mutex`] here and concurrent
//! connections serialize on it.

use alloc::boxed::Box;
use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::Duration;

use reuben_core::probe::ProbeSlots;
use reuben_document::coordinator::Coordinator;

use crate::authoring::{Diag, DocumentHistory, Report};
use crate::resources::Resources;
use crate::staged::Staged;

use super::channel::Transport;
use super::events::Bus;

mod call;
use call::{handle_call, read_tree};

use super::wire::{
    over_long_batch_refusal, over_long_probe_set_refusal, ChangedBy, Conflict, ControlMessage,
    DiagnosticsReport, DocSource, DocumentSnapshot, Event, HeldSwap, ListWhat, MetersReport,
    Request, Response, SwapReport, EMPTY_BATCH_REFUSAL, MAX_PROBES, MAX_SEND_BATCH,
};

/// What a host must provide for the window to serve the engine verbs — the device, the clock and
/// the store the window cannot know about.
///
/// The dual of the resource seam: that one is what the *engine* calls out for, this is what the
/// *channel* does. `Send + Sync` because a host serving concurrent connections calls it from
/// several threads (a swap's calls run under the Coordinator lock).
pub trait EngineHost: Send + Sync {
    /// Read a by-path swap's document text. What a path names is the host's business — a file for
    /// `reuben play`, a store key elsewhere. The error is a human message the swap turns into a
    /// rejected report.
    fn read_document(&self, path: &str) -> Result<String, String>;

    /// The source the session's resource store knows the document at `path` by. A by-path swap
    /// resolves the document's relative references from there, so the store's
    /// [`canonical`](crate::resources::Resources::canonical) must take this, with no referrer, to
    /// the document [`read_document`](Self::read_document) read.
    ///
    /// Defaults to `path` unchanged, which is right wherever the host and the store read one key
    /// space. A host whose `path` and whose store are relative to different places overrides it.
    fn document_origin(&self, path: &str) -> String {
        path.to_string()
    }

    /// Whether to hold, rather than install, a by-path swap of `path` that a client sent through
    /// [`dispatch`], so the host's user can be asked first. Called under the Coordinator lock once
    /// the `expect` guard passed and the document loaded and planned, so only a swap that would
    /// install is ever held; `true` answers [`Response::Held`] and installs nothing. Whatever
    /// asking the user means is the host's, and must not wait on them.
    ///
    /// Never consulted for a swap made through [`InProcess`], which is the host's own, nor for one
    /// by value, which names no source to ask about. Defaults to never holding.
    fn hold_swap(&self, _path: &str) -> bool {
        false
    }

    /// The names this host offers under `what`, for [`Request::List`]: instruments a
    /// [`Request::Open`] accepts, or samples a document can reference. `open` is the source of the
    /// instrument now playing, when there is one, so a host can offer what sits beside it.
    /// Defaults to nothing.
    fn offered(&self, _what: ListWhat, _open: Option<&str>) -> Vec<String> {
        Vec::new()
    }

    /// Whether a client may name `name`, a source as the store reads it, in `open` or in a `call`
    /// that reads or creates it. `open` is the source of the instrument now playing, when there is
    /// one. A document in the open instrument's own tree is always nameable; this decides the rest.
    ///
    /// Defaults to `true`: a store that is not a filesystem has nothing to confine. A host over a
    /// filesystem overrides it to keep a client to what [`offered`](Self::offered) lists.
    fn within(&self, _name: &str, _open: Option<&str>) -> bool {
        true
    }

    /// Hand a control batch to the engine's ingress **as one unit**, so a gesture cannot straddle
    /// a rendered block. `Err` means the ingress is gone (audio torn down) and nothing was queued.
    fn deliver_control(&self, messages: Vec<ControlMessage>) -> Result<(), IngressClosed>;

    /// The running counters, as an owned copy taken off the audio thread.
    fn diagnostics(&self) -> DiagnosticsReport;

    /// The levels at the device, read off the audio thread without making it wait. Defaults to
    /// empty arrays, which is the answer of a host with no device.
    fn meters(&self) -> MetersReport {
        MetersReport::default()
    }

    /// Choose which nodes' levels [`meters`](Self::meters) reports, replacing the previous choice,
    /// and return the paths that resolved to a probed node. At most [`MAX_PROBES`] paths: the
    /// window refuses more before calling this.
    ///
    /// Defaults to refusing, which is the answer of a host that cannot measure nodes.
    fn set_probes(&self, _paths: Vec<Vec<String>>) -> Result<Vec<Vec<String>>, String> {
        Err("this engine does not meter nodes".to_string())
    }

    /// The installed Plan's probe slots, handed over once for the initial install and again after
    /// every successful swap. A host that keeps its chosen paths re-resolves them against these, so
    /// the choice carries across a swap.
    fn install_probes(&self, _probes: Arc<ProbeSlots>) {}

    /// Publish the render config for the just-installed engine with `logical` output channels and
    /// `input_channels` input channels, and return any warnings to fold into the swap report.
    ///
    /// The device seam of the mailbox swap: a host with a real device rebuilds its output map here
    /// and reports what a geometry mismatch degraded; a headless one has no map to build.
    fn publish_render_config(&self, logical: usize, input_channels: usize) -> Vec<Diag>;

    /// The wait half of the deferred free: a fresh gate for one swap's reclaim poll. Each call to
    /// the returned closure waits one poll interval and answers whether to **give up**.
    ///
    /// Only the waiting is the host's — it needs a clock, a sleep, and whatever the host knows
    /// about whether its render callback is still ticking, none of which the window has. The
    /// protocol around it stays here: the window polls the Coordinator, drops the retiree off the
    /// audio thread, and treats giving up as a non-event, since the swap has already committed and
    /// the next swap's reclaim will find the retiree still in flight. A host that reimplemented
    /// that protocol could get it wrong; a host that returns a gate cannot.
    fn retire_poll(&self) -> Box<dyn FnMut() -> bool + Send + '_>;
}

/// The engine's control ingress is gone: the render callback has stopped for good, so nothing
/// downstream will ever apply the batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IngressClosed;

/// The store a [`StructureState`] serves `call` through. Its [`Staged`] is a clone of the one the
/// Coordinator resolves through, so an edit waiting in memory is what the next prepare loads.
pub type ServerStore = DocumentHistory<Staged<Box<dyn Resources + Send + Sync>>>;

/// Everything the channel answers with, cheap to clone (`Arc`-backed) so a host can hand one to
/// every connection it serves.
#[derive(Clone)]
pub struct StructureState {
    coordinator: Arc<Mutex<Coordinator>>,
    host: Arc<dyn EngineHost>,
    /// Where the playing document came from: the `source` of the last successful swap, or what the
    /// host started on. `None` for an install by value and for a built-in default rig.
    ///
    /// Door-side state, not the Coordinator's: the engine installs *document text* and has no
    /// notion of where it came from. Its own `Mutex` rather than a field behind the Coordinator's,
    /// because it is only ever written under the Coordinator lock (so the pair still advances
    /// together) and read beside it — no path takes this lock first, so the ordering cannot invert.
    installed_source: Arc<Mutex<Option<String>>>,
    store: Arc<ServerStore>,
    /// The canonical sources the last successful install read: the documents an edit may touch.
    /// Written only under the Coordinator lock, as `installed_source` is.
    tree: Arc<Mutex<BTreeSet<String>>>,
    /// Set by [`close`](StructureState::close) when the host tears the engine down.
    closed: Arc<AtomicBool>,
    events: Arc<Bus>,
}

/// What every structure verb answers once a panic has poisoned the Coordinator lock.
///
/// The poison is never cleared: the panic may have left the Coordinator between two halves of a
/// swap, and recovering the guard would hand that state to the next writer. Refusing is what keeps
/// the single writer's invariants rather than just its exclusivity.
pub const COORDINATOR_POISONED: &str =
    "the engine's Coordinator panicked mid-operation and refuses further structure requests; \
     restart the engine";

impl StructureState {
    /// Wrap a Coordinator the host built (with `install_initial`), the seam it serves through, and
    /// the store `call` reads and writes. The store's [`Staged`] must be a clone of the one handed
    /// to `install_initial`: an edit is checked by loading through the Coordinator's resolver, so
    /// a separate store's pending write would never be seen.
    pub fn new(
        coordinator: Coordinator,
        host: Arc<dyn EngineHost>,
        store: Arc<ServerStore>,
    ) -> Self {
        host.install_probes(coordinator.installed_probes());
        Self {
            coordinator: Arc::new(Mutex::new(coordinator)),
            host,
            installed_source: Arc::new(Mutex::new(None)),
            store,
            tree: Arc::new(Mutex::new(BTreeSet::new())),
            closed: Arc::new(AtomicBool::new(false)),
            events: Arc::new(Bus::default()),
        }
    }

    /// A receiver of one [`Event`] per install that goes live from now on, in install order.
    pub fn subscribe(&self) -> Receiver<Event> {
        self.events.subscribe()
    }

    /// The canonical sources the last successful install read, the top document's included. An
    /// edit to any other source is refused.
    pub fn tree(&self) -> BTreeSet<String> {
        self.tree
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Mark the engine torn down, for every clone of this state: an [`InProcess`] transport over
    /// it then fails each exchange as a dead socket would, instead of answering from a Coordinator
    /// whose render side is gone. A host calls it before it stops its server.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// Whether [`close`](Self::close) has been called on any clone.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// What is installed now, read without waiting, so a UI thread can ask on every frame.
    pub fn try_installed(&self) -> Installed {
        let coordinator = match self.coordinator.try_lock() {
            Ok(coordinator) => coordinator,
            Err(TryLockError::WouldBlock) => return Installed::Busy,
            Err(TryLockError::Poisoned(_)) => return Installed::Poisoned,
        };
        // Written only under the Coordinator lock, which is held: this never waits.
        let Ok(source) = self.installed_source() else {
            return Installed::Poisoned;
        };
        Installed::Known {
            input_channels: coordinator.installed_input_channels(),
            content_hash: coordinator.installed_hash(),
            source: source.clone(),
        }
    }

    fn coordinator(&self) -> Result<MutexGuard<'_, Coordinator>, Poisoned> {
        self.coordinator.lock().map_err(|_| Poisoned)
    }

    fn installed_source(&self) -> Result<MutexGuard<'_, Option<String>>, Poisoned> {
        self.installed_source.lock().map_err(|_| Poisoned)
    }

    fn tree_cell(&self) -> Result<MutexGuard<'_, BTreeSet<String>>, Poisoned> {
        self.tree.lock().map_err(|_| Poisoned)
    }

    /// Name the source the *initial* document was installed from. A builder step because it has a
    /// working default (`None`, a built-in rig): every later swap re-points it from the `source` it
    /// installed, so this only ever answers for the run's first document.
    ///
    /// **Replaces the cell rather than writing through it**, so the "only ever written under the
    /// Coordinator lock" invariant on that field has no exception to carve out. Taking `self` by
    /// value makes it structurally true: a builder step can only run before there is anyone to race.
    ///
    /// Also records the tree the initial document reaches, by loading it through the store, so an
    /// edit made before the first swap is checked against it.
    pub fn with_installed_source(mut self, source: Option<String>) -> Self {
        let tree = match &source {
            Some(source) => read_tree(&self.store, &self.host.document_origin(source)),
            None => BTreeSet::new(),
        };
        self.tree = Arc::new(Mutex::new(tree));
        self.installed_source = Arc::new(Mutex::new(source));
        self
    }
}

/// What [`StructureState::try_installed`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Installed {
    Known {
        /// The installed engine's logical input channel count.
        input_channels: usize,
        content_hash: String,
        /// Where the installed document came from, as `get_document` reports it.
        source: Option<String>,
    },
    /// Another caller holds the Coordinator, such as a swap building its engine: ask again shortly.
    Busy,
    /// A panic poisoned the Coordinator; it answers nothing from here on.
    Poisoned,
}

/// A lock a panic poisoned; answered as [`COORDINATOR_POISONED`].
struct Poisoned;

impl From<Poisoned> for Response {
    fn from(_: Poisoned) -> Self {
        Response::Error {
            message: COORDINATOR_POISONED.to_string(),
        }
    }
}

/// Dispatch one request line to its response. Pure over [`StructureState`], so a host can drive it
/// without a socket and the framing loop stays the host's.
///
/// A panic under the Coordinator lock unwinds out of this call and poisons the lock; every later
/// structure verb, from any door, then answers [`COORDINATOR_POISONED`] as a channel error.
///
/// An unreadable line is a channel-level [`Response::Error`] (distinct from a domain answer that
/// reports failure), so a malformed request still gets exactly one framed reply and the
/// one-response-per-request invariant holds.
///
/// This is a client's entry, so a by-path swap is offered to [`EngineHost::hold_swap`] first.
/// [`InProcess`], the host's own transport, never is.
pub fn dispatch(state: &StructureState, line: &str) -> Response {
    match Request::from_ndjson(line) {
        Ok(request) => dispatch_request(state, request),
        Err(e) => unreadable(e),
    }
}

/// [`dispatch`] for a request the host has already parsed — to see whether it is a
/// [`Request::Subscribe`] its connection must stream for — so it is not parsed twice.
pub fn dispatch_request(state: &StructureState, request: Request) -> Response {
    serve(state, request, Caller::Client)
}

fn unreadable(e: serde_json::Error) -> Response {
    Response::Error {
        message: format!("unreadable request: {e}"),
    }
}

/// Who sent a request: a client of the host's server, whose swap the host may hold, or the host
/// itself through [`InProcess`].
#[derive(Clone, Copy, PartialEq, Eq)]
enum Caller {
    Client,
    Host,
}

fn serve(state: &StructureState, request: Request, caller: Caller) -> Response {
    match request {
        Request::Ping => Response::Pong,
        Request::GetDocument => get_document(state).unwrap_or_else(Response::from),
        Request::GetDiagnostics => Response::Diagnostics(state.host.diagnostics()),
        Request::GetMeters => Response::Meters(state.host.meters()),
        Request::SetProbes { paths } => handle_set_probes(state, paths),
        Request::Swap { source, expect } => handle_swap(state, source, expect, caller),
        Request::Send { messages } => handle_send(state, messages),
        Request::Call { tool, args } => handle_call(state, &tool, args),
        Request::Open { name, expect } => handle_open(state, name, expect, caller),
        Request::List { what } => handle_list(state, what),
        Request::Subscribe => Response::Error {
            message: UNSTREAMED_SUBSCRIBE.to_string(),
        },
    }
}

/// What a `subscribe` answers when it reaches [`dispatch`] rather than a connection that streams.
const UNSTREAMED_SUBSCRIBE: &str = "subscribe needs a connection that streams events; this one \
     answers one response per request";

/// The Coordinator owns the canonical document; serialize it + its hash under the lock so
/// `get_document` never sees a half-installed pair.
fn get_document(state: &StructureState) -> Result<Response, Poisoned> {
    let coordinator = state.coordinator()?;
    let document = serde_json::to_value(&**coordinator.document())
        .expect("canonical instrument document serializes to JSON");
    Ok(Response::Document(DocumentSnapshot {
        document,
        content_hash: coordinator.installed_hash(),
        // Read under the Coordinator lock too, so the source and the hash a caller compares it
        // against can never come from different installs.
        source: state.installed_source()?.clone(),
    }))
}

/// The structure channel with no socket under it: a [`Transport`] that hands each request line to
/// the same dispatch a socket client reaches, on the caller's thread, so a host embedding the
/// engine drives the same verbs over the same [`StructureState`] its loopback server serves.
///
/// The request still crosses the NDJSON envelope. That is the point, not an overhead to optimize
/// away: the framing and the response classification are then the ones a socket client gets, so an
/// in-process caller and the MCP sidecar cannot see different answers to the same request — with
/// the one exception the host asks for: its own swaps are never held ([`EngineHost::hold_swap`]).
///
/// `round_trip` fails in exactly the two cases a socket client would see a dead peer: the host has
/// [closed](StructureState::close) the state (the engine is torn down), or the request panicked —
/// which is caught here rather than unwound into the caller's thread, the way a panicking server
/// thread only drops its connection. Both are an [`io::Error`](std::io::Error), so the channel
/// reports the engine unreachable. It ignores `read_timeout`, there being no peer to go silent:
/// what bounds a call is `dispatch` itself — a swap blocks on the Coordinator lock behind any
/// in-flight swap, and its reclaim poll is bounded by the host's [`EngineHost::retire_poll`] gate.
#[derive(Clone)]
pub struct InProcess {
    state: StructureState,
}

impl InProcess {
    /// A transport over `state` — typically a clone of the one the host's server also serves.
    pub fn new(state: StructureState) -> Self {
        Self { state }
    }

    /// A receiver of one [`Event`] per install that goes live from now on, in install order.
    pub fn subscribe(&self) -> Receiver<Event> {
        self.state.subscribe()
    }
}

impl core::fmt::Debug for InProcess {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("InProcess")
    }
}

impl Transport for InProcess {
    fn round_trip(&self, line: &str, _read_timeout: Duration) -> std::io::Result<String> {
        if self.state.is_closed() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "the engine has shut down",
            ));
        }
        // Unwind-safe in the sense that matters: the only state a panic can leave half-written
        // is behind the Coordinator lock, which the panic poisons and `dispatch` then refuses.
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || match Request::from_ndjson(line) {
                Ok(request) => serve(&self.state, request, Caller::Host),
                Err(e) => unreadable(e),
            },
        ))
        .map(|response| response.to_ndjson())
        .map_err(|_| std::io::Error::other("the engine panicked answering the request"))
    }

    fn endpoint(&self) -> &str {
        IN_PROCESS_ENDPOINT
    }
}

/// What [`InProcess::endpoint`] reports: not an address, since nothing is dialed.
pub const IN_PROCESS_ENDPOINT: &str = "in-process";

/// Control traffic: hand the batch to the host's ingress **as one unit** and ack it.
///
/// Deliberately thin — there is **no routing logic here**. The engine types and routes each message
/// against its destination port exactly as it would an external datagram, and an address matching
/// no node/port is dropped there, silently, which is why the ack is "queued", never "applied".
///
/// The Coordinator is untouched: control is not structure, so a `send` never contends for the
/// single-writer lock and cannot be starved by an in-flight swap's bounded reclaim poll.
fn handle_send(state: &StructureState, messages: Vec<ControlMessage>) -> Response {
    // Bound the batch before it is queued. Its whole cost lands in one render callback, so an
    // unbounded batch is an RT hazard, not merely a large request. The client checks both bounds
    // too and saves the round trip; this is the check that has to hold, because a door is not
    // obliged to have made the first one. Both say it in the same words.
    if messages.is_empty() {
        return Response::Error {
            message: EMPTY_BATCH_REFUSAL.to_string(),
        };
    }
    if messages.len() > MAX_SEND_BATCH {
        return Response::Error {
            message: over_long_batch_refusal(messages.len()),
        };
    }

    match state.host.deliver_control(messages) {
        // Nothing was queued — the batch was one delivery — so this is a clean all-or-nothing
        // failure, and saying so beats acking a lie.
        Err(_) => Response::Error {
            message: "the engine's control ingress is closed; is audio still running?".to_string(),
        },
        Ok(()) => Response::Sent,
    }
}

/// Choose the probed nodes. Bounded before the host is asked, so a client cannot make one render
/// callback fold an unbounded number of nodes.
fn handle_set_probes(state: &StructureState, paths: Vec<Vec<String>>) -> Response {
    if paths.len() > MAX_PROBES {
        return Response::Error {
            message: over_long_probe_set_refusal(),
        };
    }
    match state.host.set_probes(paths) {
        Ok(resolved) => Response::Probes { resolved },
        Err(message) => Response::Error { message },
    }
}

/// The mailbox-swap install path. Everything runs under the Coordinator lock so the
/// `expect`-compare and the swap are one atomic critical section (a compare-and-swap) — concurrent
/// swaps from multiple connections serialize, and neither `get_document` nor another swap sees a
/// half-installed document. In order:
///
/// 1. **Resolve** the source to its JSON text — inline JSON re-serialized, or a read through the
///    host, which also names where a path's document resolves its own references from. A read
///    failure is a rejected report (no install, prior retained), not a channel error.
/// 2. **Arbitrate**: a stale `expect` rejects with the real installed hash as [`Response::Conflict`]
///    and does **not** swap. Absent `expect` is last-write-wins. Done here rather than inside the
///    Coordinator's swap, which owns no guard at all, by design.
/// 3. **Swap**: the Coordinator validates + builds a whole new Engine off-thread, fills the install
///    mailbox, and returns the real report. A load/plan error aborts with `ok: false` and the prior
///    hash — the old engine keeps playing (retain-prior). A client's by-path swap that validated
///    is offered to [`EngineHost::hold_swap`] between the build and the install; a held one is
///    dropped and answers [`Response::Held`].
/// 4. **Publish** the render config for the new engine's geometry and fold any warning into the
///    report — BEFORE the reclaim, so the map is in flight before the callback installs the new
///    engine and the two mailboxes stay in lockstep.
/// 5. **Reclaim** the retired Engine off-thread, clearing the mailbox for the next swap.
fn handle_swap(
    state: &StructureState,
    source: DocSource,
    expect: Option<String>,
    caller: Caller,
) -> Response {
    // Name the source before resolving consumes it — recorded only if the install below succeeds, so
    // a rejected swap leaves `get_document` still naming what is actually playing. An install by
    // value has no source to name.
    let installed_from = match &source {
        DocSource::Path(path) => Some(path.clone()),
        DocSource::Document(_) => None,
    };

    let json = match resolve_source(source, state.host.as_ref()) {
        Ok(json) => json,
        Err(message) => return rejected_swap(state, message),
    };

    let mut coordinator = match state.coordinator() {
        Ok(coordinator) => coordinator,
        Err(poisoned) => return poisoned.into(),
    };
    install_from(
        state,
        &mut coordinator,
        &json,
        installed_from,
        expect,
        caller,
        Went::Swapped,
    )
}

/// Steps 2 to 5 of [`handle_swap`], for a document already read: the `expect` guard, the build, the
/// hold, the install and everything after it. `installed_from` is the path the document was read
/// from, `None` for one by value.
fn install_from(
    state: &StructureState,
    coordinator: &mut Coordinator,
    json: &str,
    installed_from: Option<String>,
    expect: Option<String>,
    caller: Caller,
    went: Went,
) -> Response {
    let origin = installed_from
        .as_deref()
        .map(|path| state.host.document_origin(path));

    if let Some(expected) = &expect {
        let actual = coordinator.installed_hash();
        if expected != &actual {
            return Response::Conflict(Conflict {
                expected: expected.clone(),
                actual,
            });
        }
    }

    // Unguarded by construction: arbitration was settled above.
    let (core_report, tree) = match &origin {
        Some(origin) => {
            // The one-call swap's own first step, kept so a build here peaks at live + new.
            drop(coordinator.try_reclaim());
            let (prepared, tree) = prepare_recording(state, coordinator, json, origin);
            match prepared {
                // A document that does not load is rejected with its errors, never held: the
                // user is only asked about something that would install.
                Err(rejected) => (rejected, tree),
                Ok(prepared) => {
                    let holds = caller == Caller::Client
                        && installed_from
                            .as_deref()
                            .is_some_and(|path| state.host.hold_swap(path));
                    if holds {
                        drop(prepared);
                        let playing = match state.installed_source() {
                            Ok(source) => source.clone(),
                            Err(poisoned) => return poisoned.into(),
                        };
                        return Response::Held(HeldSwap {
                            requested: installed_from.unwrap_or_default(),
                            playing,
                            content_hash: coordinator.installed_hash(),
                        });
                    }
                    (coordinator.commit_swap(prepared), tree)
                }
            }
        }
        // By value there is nowhere the document came from: the session's anchor stands, and no
        // document is open to edit.
        None => (coordinator.swap_document(json), BTreeSet::new()),
    };
    let mut report = SwapReport::from_core(core_report);
    if report.report.ok {
        match went_live(state, coordinator, installed_from, tree, went) {
            Ok(warnings) => report.report.warnings.extend(warnings),
            Err(poisoned) => return poisoned.into(),
        }
    }
    Response::SwapReport(report)
}

/// Unload the open instrument and load, and play, the one `name` names: [`handle_swap`] by path,
/// read through the server's store so the tree and the edits that follow see the same files.
///
/// Under the Coordinator lock from the read on, so the `discard` cannot drop another request's
/// pending write.
fn handle_open(
    state: &StructureState,
    name: String,
    expect: Option<String>,
    caller: Caller,
) -> Response {
    let mut coordinator = match state.coordinator() {
        Ok(coordinator) => coordinator,
        Err(poisoned) => return poisoned.into(),
    };
    state.store.inner().discard();
    let origin = state.host.document_origin(&name);
    let open = match state.installed_source() {
        Ok(source) => source.clone(),
        Err(poisoned) => return poisoned.into(),
    };
    if !state.host.within(&origin, open.as_deref()) {
        return rejected_report(coordinator.installed_hash(), not_offered(&name));
    }
    let json = match state.store.read_text(&origin) {
        Ok(json) => json,
        Err(e) => {
            return rejected_report(coordinator.installed_hash(), format!("open {name}: {e}"))
        }
    };
    install_from(
        state,
        &mut coordinator,
        &json,
        Some(name),
        expect,
        caller,
        Went::Opened,
    )
}

/// The refusal for a name outside what the host offers: the client's own spelling and a fixed
/// reason, so it says nothing of the server's paths or whether the file exists.
fn not_offered(name: &str) -> String {
    format!("`{name}` is not offered by this server")
}

/// What the host offers under `what`, each name one [`Request::Open`] (or, for a sample, a
/// resource reference) accepts back.
fn handle_list(state: &StructureState, what: ListWhat) -> Response {
    let open = match state.installed_source() {
        Ok(source) => source.clone(),
        Err(poisoned) => return poisoned.into(),
    };
    Response::Listed {
        names: state.host.offered(what, open.as_deref()),
    }
}

/// Prepare `json`, read from `origin`, and the canonical sources the load read, `origin`'s own
/// included.
fn prepare_recording(
    state: &StructureState,
    coordinator: &Coordinator,
    json: &str,
    origin: &str,
) -> (
    Result<reuben_document::PreparedSwap, reuben_document::SwapReport>,
    BTreeSet<String>,
) {
    let log = state.store.inner().record_reads();
    let prepared = coordinator.prepare_document_from(json, origin);
    let mut tree = log.finish();
    tree.insert(state.store.canonical(origin, None));
    (prepared, tree)
}

/// Which verb made an install that went live, which decides the [`Event`] it publishes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Went {
    /// `open`: always [`Event::Opened`].
    Opened,
    /// `swap`: [`Event::Opened`] when it names a source other than the one playing before, by
    /// value counting as a source of its own, and [`Event::Changed`] when it names the same one.
    Swapped,
    /// An edit through `call`: always [`Event::Changed`].
    Edited,
}

/// Everything after a committed install, shared by `swap`, `open` and an edit made through `call`:
/// the source and tree that name what plays advance, the host gets the new geometry and probes,
/// the retiree is reclaimed, and subscribers hear of it. Returns the host's render-config warnings
/// for the report.
///
/// Under the Coordinator lock, so the source, the tree and the installed hash never disagree
/// about which install a reader is looking at, and events publish in install order. The render
/// config is published BEFORE the reclaim, so the map is in flight before the callback installs
/// the new engine and the two mailboxes stay in lockstep.
fn went_live(
    state: &StructureState,
    coordinator: &mut Coordinator,
    source: Option<String>,
    tree: BTreeSet<String>,
    went: Went,
) -> Result<Vec<Diag>, Poisoned> {
    let previous = core::mem::replace(&mut *state.installed_source()?, source.clone());
    *state.tree_cell()? = tree;

    let warnings = state.host.publish_render_config(
        coordinator.installed_channels(),
        coordinator.installed_input_channels(),
    );
    state.host.install_probes(coordinator.installed_probes());

    reclaim_retired(coordinator, state.host.as_ref());

    let hash = coordinator.installed_hash();
    let opened = match went {
        Went::Opened => true,
        Went::Swapped => source != previous,
        Went::Edited => false,
    };
    state.events.publish(if opened {
        Event::Opened {
            name: source.unwrap_or_default(),
            hash,
        }
    } else {
        Event::Changed {
            hash,
            by: ChangedBy::Client,
        }
    });
    Ok(warnings)
}

/// Take the retired Engine back from the render side and **drop it here, off the audio thread** —
/// the deferred free — polling over the gate the host supplies.
///
/// A timeout is not fatal and deliberately not reported: the swap has already committed, so giving
/// up only leaves the retiree in flight for the next swap's reclaim (the "audio isn't consuming
/// swaps" case). The drop is what frees the retired Engine, and it happens on this thread.
fn reclaim_retired(coordinator: &mut Coordinator, host: &dyn EngineHost) {
    let mut give_up = host.retire_poll();
    match coordinator.reclaim(&mut give_up) {
        Ok(retiree) => drop(retiree),
        Err(_) => { /* audio not consuming yet; the next swap or shutdown reclaims it */ }
    }
}

/// A rejected swap that never reached the Coordinator (a source read failure): `ok: false`, the
/// message, no diff, and the still-installed hash — the report names what keeps playing
/// (retain-prior).
fn rejected_swap(state: &StructureState, message: String) -> Response {
    match state.coordinator() {
        Ok(coordinator) => rejected_report(coordinator.installed_hash(), message),
        Err(poisoned) => poisoned.into(),
    }
}

fn rejected_report(content_hash: String, message: String) -> Response {
    Response::SwapReport(SwapReport {
        report: Report {
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

/// Resolve a swap's source to its JSON text: inline JSON re-serialized, or a read through the host.
/// A failure here is a human message the caller turns into a rejected swap.
fn resolve_source(source: DocSource, host: &dyn EngineHost) -> Result<String, String> {
    match source {
        DocSource::Document(value) => serde_json::to_string(&value)
            .map_err(|e| format!("serialize inline swap document: {e}")),
        DocSource::Path(path) => host.read_document(&path),
    }
}

#[cfg(all(test, feature = "render"))]
mod tests {
    use super::*;
    use crate::authoring::{DocumentHistory, ResolveError, Resources, SampleBuffer};
    use crate::engine::{get_engine_status, Channel, ChannelError};
    use crate::staged::Staged;

    const DOC: &str = r#"{"format_version":3,"instrument":"t",
        "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

    struct NoStore;

    impl Resources for NoStore {
        fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(source.to_string()))
        }
    }

    /// A host that panics under the Coordinator lock — `publish_render_config` runs inside a swap's
    /// critical section — when `panic_on_publish` is set.
    struct Host {
        panic_on_publish: bool,
        /// Holds every client swap it is offered, and counts the offers.
        hold: bool,
        offered: std::sync::atomic::AtomicUsize,
        /// Counts `set_probes` calls.
        probed: std::sync::atomic::AtomicUsize,
        /// Counts `install_probes` calls.
        installed: std::sync::atomic::AtomicUsize,
        /// The slots `install_probes` was last given.
        last_installed: Mutex<Option<Arc<ProbeSlots>>>,
        /// Whether `set_probes` is the node-metering host's, answering every path as resolved.
        meters_nodes: bool,
    }

    /// The one path [`Host`] reads [`DOC`] from.
    const PATH: &str = "t.json";

    impl EngineHost for Host {
        fn read_document(&self, path: &str) -> Result<String, String> {
            if path == PATH {
                return Ok(DOC.to_string());
            }
            Err(format!("no file {path}"))
        }
        fn hold_swap(&self, _path: &str) -> bool {
            self.offered.fetch_add(1, Ordering::SeqCst);
            self.hold
        }
        fn deliver_control(&self, _: Vec<ControlMessage>) -> Result<(), IngressClosed> {
            Ok(())
        }
        fn set_probes(&self, paths: Vec<Vec<String>>) -> Result<Vec<Vec<String>>, String> {
            self.probed.fetch_add(1, Ordering::SeqCst);
            if self.meters_nodes {
                Ok(paths)
            } else {
                Err("this engine does not meter nodes".to_string())
            }
        }
        fn install_probes(&self, slots: Arc<ProbeSlots>) {
            *self.last_installed.lock().unwrap() = Some(slots);
            self.installed.fetch_add(1, Ordering::SeqCst);
        }
        fn diagnostics(&self) -> DiagnosticsReport {
            DiagnosticsReport::default()
        }
        fn publish_render_config(&self, _: usize, _: usize) -> Vec<Diag> {
            assert!(!self.panic_on_publish, "host panicked mid-swap");
            Vec::new()
        }
        fn retire_poll(&self) -> Box<dyn FnMut() -> bool + Send + '_> {
            Box::new(|| true)
        }
    }

    fn state(panic_on_publish: bool) -> StructureState {
        state_with(Host {
            panic_on_publish,
            hold: false,
            offered: Default::default(),
            probed: Default::default(),
            installed: Default::default(),
            last_installed: Default::default(),
            meters_nodes: false,
        })
        .0
    }

    fn state_with(host: Host) -> (StructureState, Arc<Host>) {
        let (coordinator, _side, _) =
            crate::engine::install_initial(DOC, NoStore, reuben_core::AudioConfig::new(48e3, 128))
                .expect("install");
        let host = Arc::new(host);
        let store = Arc::new(DocumentHistory::new(Staged::new(
            Box::new(NoStore) as Box<dyn Resources + Send + Sync>
        )));
        let state =
            StructureState::new(coordinator, Arc::clone(&host) as Arc<dyn EngineHost>, store)
                .with_installed_source(Some("playing.json".to_string()));
        (state, host)
    }

    fn holding() -> (StructureState, Arc<Host>) {
        state_with(Host {
            panic_on_publish: false,
            hold: true,
            offered: Default::default(),
            probed: Default::default(),
            installed: Default::default(),
            last_installed: Default::default(),
            meters_nodes: false,
        })
    }

    fn set_probes_line(count: usize) -> String {
        Request::SetProbes {
            paths: (0..count).map(|i| vec![format!("n{i}")]).collect(),
        }
        .to_ndjson()
    }

    #[test]
    fn set_probes_past_the_cap_is_refused_without_reaching_the_host() {
        let (state, host) = state_with(Host {
            panic_on_publish: false,
            hold: false,
            offered: Default::default(),
            probed: Default::default(),
            installed: Default::default(),
            last_installed: Default::default(),
            meters_nodes: true,
        });
        match dispatch(&state, &set_probes_line(MAX_PROBES + 1)) {
            Response::Error { message } => assert_eq!(message, "at most 512 probes"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(host.probed.load(Ordering::SeqCst), 0);
        match dispatch(&state, &set_probes_line(MAX_PROBES)) {
            Response::Probes { resolved } => assert_eq!(resolved.len(), MAX_PROBES),
            other => panic!("expected the resolved paths, got {other:?}"),
        }
    }

    #[test]
    fn a_host_without_node_meters_says_so() {
        let state = state(false);
        match dispatch(&state, &set_probes_line(1)) {
            Response::Error { message } => {
                assert_eq!(message, "this engine does not meter nodes")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn the_host_is_given_the_installed_probes_at_the_start_and_after_each_swap() {
        let (state, host) = holding();
        assert_eq!(host.installed.load(Ordering::SeqCst), 1);
        let channel = Channel::new(InProcess::new(state.clone()));
        swap_by_value(&channel).expect("swap");
        assert_eq!(host.installed.load(Ordering::SeqCst), 2);
        let given = host.last_installed.lock().unwrap().clone().expect("given");
        let coordinator = state.coordinator.lock().unwrap();
        assert!(Arc::ptr_eq(&given, &coordinator.installed_probes()));
    }

    fn swap_by_path(expect: Option<String>) -> String {
        Request::Swap {
            source: DocSource::Path(PATH.to_string()),
            expect,
        }
        .to_ndjson()
    }

    #[test]
    fn a_client_swap_the_host_holds_installs_nothing_and_names_what_plays() {
        let (state, _host) = holding();
        let before = state.try_installed();
        match dispatch(&state, &swap_by_path(None)) {
            Response::Held(held) => {
                assert_eq!(held.requested, PATH);
                assert_eq!(held.playing.as_deref(), Some("playing.json"));
                let Installed::Known { content_hash, .. } = &before else {
                    panic!("{before:?}");
                };
                assert_eq!(&held.content_hash, content_hash);
            }
            other => panic!("expected a held swap, got {other:?}"),
        }
        assert_eq!(state.try_installed(), before);
    }

    #[test]
    fn the_hosts_own_swap_is_never_offered_for_holding() {
        let (state, host) = holding();
        let channel = Channel::new(InProcess::new(state.clone()));
        match channel.swap(DocSource::Path(PATH.to_string()), None) {
            Ok(super::super::SwapOutcome::Installed(report)) => {
                assert!(report.report.ok, "{:?}", report.report.errors)
            }
            other => panic!("expected an install, got {other:?}"),
        }
        assert_eq!(host.offered.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_stale_expect_conflicts_before_the_host_is_asked_to_hold() {
        let (state, host) = holding();
        match dispatch(&state, &swap_by_path(Some("0badc0de".to_string()))) {
            Response::Conflict(conflict) => assert_eq!(conflict.expected, "0badc0de"),
            other => panic!("expected a conflict, got {other:?}"),
        }
        assert_eq!(host.offered.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_unreadable_source_is_rejected_before_the_host_is_asked_to_hold() {
        let (state, host) = holding();
        let line = Request::Swap {
            source: DocSource::Path("missing.json".to_string()),
            expect: None,
        }
        .to_ndjson();
        match dispatch(&state, &line) {
            Response::SwapReport(report) => assert!(!report.report.ok),
            other => panic!("expected a rejected report, got {other:?}"),
        }
        assert_eq!(host.offered.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_swap_by_value_names_no_source_to_hold() {
        let (state, host) = holding();
        let line = Request::Swap {
            source: DocSource::Document(serde_json::from_str(DOC).expect("doc json")),
            expect: None,
        }
        .to_ndjson();
        match dispatch(&state, &line) {
            Response::SwapReport(report) => assert!(report.report.ok),
            other => panic!("expected an install, got {other:?}"),
        }
        assert_eq!(host.offered.load(Ordering::SeqCst), 0);
    }

    fn swap_by_value(channel: &Channel) -> Result<super::super::SwapOutcome, ChannelError> {
        channel.swap(
            DocSource::Document(serde_json::from_str(DOC).expect("doc json")),
            None,
        )
    }

    #[test]
    fn a_panic_under_the_lock_is_unreachable_then_every_verb_refuses_without_panicking() {
        let state = state(true);
        let channel = Channel::new(InProcess::new(state.clone()));

        let first = swap_by_value(&channel).expect_err("the panicking swap has no answer");
        assert!(first.is_unreachable(), "{first}");

        // Poisoned now: in-process and over any other door, an error rather than a panic.
        for err in [
            swap_by_value(&channel).expect_err("poisoned"),
            channel.get_document().expect_err("poisoned"),
        ] {
            match err {
                ChannelError::Channel(message) => assert_eq!(message, COORDINATOR_POISONED),
                other => panic!("expected the poisoned refusal, got {other}"),
            }
        }
        match dispatch(&state, &Request::GetDocument.to_ndjson()) {
            Response::Error { message } => assert_eq!(message, COORDINATOR_POISONED),
            other => panic!("expected the poisoned refusal, got {other:?}"),
        }
    }

    #[test]
    fn subscribe_through_dispatch_is_a_channel_error() {
        let state = state(false);
        match dispatch(&state, &Request::Subscribe.to_ndjson()) {
            Response::Error { message } => assert_eq!(message, UNSTREAMED_SUBSCRIBE),
            other => panic!("expected a channel error, got {other:?}"),
        }
    }

    #[test]
    fn a_closed_state_is_unreachable_in_process() {
        let state = state(false);
        let channel = Channel::new(InProcess::new(state.clone()));
        assert!(get_engine_status(&channel, "test").output.reachable);
        state.close();
        assert!(!get_engine_status(&channel, "test").output.reachable);
        assert!(swap_by_value(&channel)
            .expect_err("closed")
            .is_unreachable());
    }
}
