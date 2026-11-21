//! The serving side of the structure channel: what each verb *means*, over the seam a host fills.
//!
//! One [`Response`] per [`Request`], in order. There is no socket here and no thread — a host owns
//! the listener, the framing loop and the shutdown; this is the part that would otherwise be
//! rewritten per host, and rewritten differently.
//!
//! The verbs:
//! - [`Request::Ping`] → [`Response::Pong`] — liveness of the channel itself.
//! - [`Request::GetDocument`] → what plays, by source and content hash, never its text. It changes
//!   when an open, an accepted edit or an outside edit to the open instrument installs a new one.
//! - [`Request::GetDiagnostics`] → the host's counters, snapshotted off-thread.
//! - [`Request::GetMeters`] → the host's levels at the device, read the same way.
//! - [`Request::SetProbes`] → which nodes `GetMeters` also reports, chosen through the host.
//! - [`Request::Send`] → control traffic, handed to the host's ingress as **one** batch. The one
//!   verb that does not touch the Coordinator: control is not structure.
//! - [`Request::Call`] → one non-engine roster verb against the engine's store. A read answers
//!   directly. An edit waits in memory while the whole open instrument loads and plans with it;
//!   only then is it written and the new Plan swapped in, and otherwise nothing is written and the
//!   answer says why. The exception is an open instrument that does not load: nothing plays, and
//!   an edit is written whether or not it makes the instrument load.
//! - [`Request::Open`] → unload what plays and play the named document, by a **mailbox swap**: the
//!   Coordinator validates and builds a whole new Engine off-thread, fills the install mailbox, and
//!   returns a real report with survivor/reset stats. The `expect` guard is applied inside the
//!   Coordinator lock, so the compare and the swap are one critical section. A document that does
//!   not load still opens, and nothing plays until an edit makes it load.
//!
//! A client changes what plays only through `Open` and `Call`: no request carries a document.
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
use alloc::collections::{BTreeMap, BTreeSet};
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
use crate::staged::{Reads, Staged};

use super::channel::Transport;
use super::events::{Bus, Subscription};

mod call;
use call::{diag, handle_call, handle_library, read_tree};

use super::wire::{
    over_long_batch_refusal, over_long_probe_set_refusal, ChangedBy, Conflict, ControlMessage,
    DiagnosticsReport, DocumentSnapshot, Event, HeldSwap, ListWhat, MetersReport, Request,
    Response, SwapReport, EMPTY_BATCH_REFUSAL, MAX_PROBES, MAX_SEND_BATCH,
};

/// What a host must provide for the window to serve the engine verbs — the device, the clock and
/// the store the window cannot know about.
///
/// The dual of the resource seam: that one is what the *engine* calls out for, this is what the
/// *channel* does. `Send + Sync` because a host serving concurrent connections calls it from
/// several threads (a swap's calls run under the Coordinator lock).
pub trait EngineHost: Send + Sync {
    /// The source the session's resource store knows the document at `path` by. An open reads the
    /// document there and resolves its relative references from there, so the store's
    /// [`canonical`](crate::resources::Resources::canonical) must take this, with no referrer, to
    /// the document `path` names.
    ///
    /// Defaults to `path` unchanged, which is right wherever the host and the store read one key
    /// space. A host whose `path` and whose store are relative to different places overrides it.
    fn document_origin(&self, path: &str) -> String {
        path.to_string()
    }

    /// Whether to hold, rather than install, an open of `path` that a client sent through
    /// [`dispatch`], so the host's user can be asked first. Called under the Coordinator lock once
    /// the `expect` guard passed, for any open, since an open of a document that does not load
    /// still unloads what plays. `true` answers [`Response::Held`] and installs nothing. Whatever
    /// asking the user means is the host's, and must not wait on them.
    ///
    /// Never consulted for an open made through [`InProcess`], which is the host's own. Defaults to
    /// never holding.
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

    /// The instruments a start screen offers, for [`Request::Library`]: names a [`Request::Open`]
    /// accepts. `open` is the source of the instrument now playing, when there is one. Defaults to
    /// nothing.
    fn library(&self, _open: Option<&str>) -> Vec<String> {
        Vec::new()
    }

    /// The directories [`Library::dirs`](super::wire::Library::dirs) reports, where a client may create an instrument. `open`
    /// is as for [`library`](Self::library). Defaults to nothing.
    fn creatable(&self, _open: Option<&str>) -> Vec<String> {
        Vec::new()
    }

    /// The text of the surface document of `open`, the source of the instrument now playing, or
    /// `None` when it has none. `Err` is a surface that exists and cannot be read. Defaults to
    /// `Ok(None)`.
    fn surface(&self, _open: &str) -> Result<Option<String>, String> {
        Ok(None)
    }

    /// Whether a client may name `name`, a source as the store reads it, in `open` or in a `call`
    /// that reads or creates it. `open` is the source of the instrument now playing, when there is
    /// one. A document in the open instrument's own tree is always nameable, one it references and
    /// could not read included; this decides the rest.
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
    /// What the last successful install read or referenced: the documents an edit may touch.
    /// Written only under the Coordinator lock, as `installed_source` is.
    tree: Arc<Mutex<Tree>>,
    /// Set by [`close`](StructureState::close) when the host tears the engine down.
    closed: Arc<AtomicBool>,
    events: Arc<Bus>,
    /// The errors of the last [`Event::Invalid`] a [`reload`](StructureState::reload) published,
    /// until something installs or a reload finds the files plan again. Written only under the
    /// Coordinator lock.
    reported_invalid: Arc<Mutex<Option<Vec<Diag>>>>,
    /// Set while the open instrument does not load and [`SILENCE`] plays in its place. Written only
    /// under the Coordinator lock.
    silent: Arc<AtomicBool>,
}

/// What plays while the open instrument does not load, or while nothing is open: one output
/// channel that is always zero. Installed by value through the ordinary swap, so the render side
/// sees nothing new. A host starting with nothing open builds its Coordinator from it and marks the
/// state [`with_nothing_open`](StructureState::with_nothing_open).
pub const SILENCE: &str = r#"{"format_version":3,"instrument":"silence",
    "interface":{"outputs":{"out":{"from":"/silence.audio"}}},
    "nodes":[{"type":"output","address":"/silence"}]}"#;

/// The content hash every answer reports while [`SILENCE`] plays: no document's hash names it.
const NOTHING_PLAYS: &str = "";

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
            tree: Arc::new(Mutex::new(Tree::default())),
            closed: Arc::new(AtomicBool::new(false)),
            events: Arc::new(Bus::default()),
            reported_invalid: Arc::new(Mutex::new(None)),
            silent: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The hash a client is told plays, and the one an `expect` is compared against: the installed
    /// hash, or [`NOTHING_PLAYS`] while the open instrument does not load. Called under the
    /// Coordinator lock.
    fn playing_hash(&self, coordinator: &Coordinator) -> String {
        if self.is_silent() {
            NOTHING_PLAYS.to_string()
        } else {
            coordinator.installed_hash()
        }
    }

    fn is_silent(&self) -> bool {
        self.silent.load(Ordering::SeqCst)
    }

    /// Load the open instrument again from its files, after something outside the engine may have
    /// written them, and play it if it plans and differs from what plays.
    ///
    /// [`try_reload`](Self::try_reload), with an earlier swap still in flight answered as `None`:
    /// for a caller that will look again on the next touch of the files anyway.
    pub fn reload(&self) -> Option<Event> {
        self.try_reload().unwrap_or(None)
    }

    /// Load the open instrument again from its files, after something outside the engine may have
    /// written them, and play it if it plans and differs from what plays.
    ///
    /// It differs when the top document's content hash does, or the text of any document it
    /// reaches does: the hash covers only the top one.
    ///
    /// - An earlier swap is still in flight: nothing is read, and [`SwapInFlight`] says to ask
    ///   again shortly. A swap stays in flight only while the render side is not taking swaps
    ///   home.
    /// - It plans and differs: it goes live and [`Event::Changed`] by [`ChangedBy::Outside`] is
    ///   published and returned.
    /// - It does not load or plan: what played keeps playing, and [`Event::Invalid`] is published
    ///   and returned, once per distinct set of errors. While the open instrument already did not
    ///   load, the documents it now reaches become the ones an edit may touch.
    /// - It plans while nothing plays because it did not load before: it goes live as above.
    /// - It plans and is what plays, after an [`Event::Invalid`]: [`Event::Changed`] by
    ///   [`ChangedBy::Outside`] with the installed hash is published and returned, so a subscriber
    ///   hears the failure is over.
    /// - Otherwise `None`, and nothing is published: the files are what plays (the engine's own
    ///   write reads back this way), nothing is open from a source, or the Coordinator is
    ///   poisoned.
    ///
    /// Under the Coordinator lock throughout, so the `discard` cannot drop another request's
    /// pending write and the files are read once against one installed hash.
    pub fn try_reload(&self) -> Result<Option<Event>, SwapInFlight> {
        let Ok(mut coordinator) = self.coordinator() else {
            return Ok(None);
        };
        self.store.inner().discard();
        let Some(top) = self.installed_source().ok().and_then(|top| top.clone()) else {
            return Ok(None);
        };
        // Reclaims a retiree that has come home, which ends its swap's flight and keeps the build
        // below at live + new, as a by-path swap's does.
        if coordinator.swap_in_flight() {
            return Err(SwapInFlight);
        }
        let origin = self.host.document_origin(&top);
        let text = match self.store.read_text(&origin) {
            Ok(text) => text,
            Err(e) => return Ok(self.invalid(vec![diag(format!("{top} could not be read: {e}"))])),
        };
        let (prepared, tree) = prepare_recording(self, &coordinator, &text, &origin);
        let prepared = match prepared {
            Ok(prepared) => prepared,
            Err(rejected) => {
                if self.is_silent() {
                    let Ok(mut tree_cell) = self.tree_cell() else {
                        return Ok(None);
                    };
                    *tree_cell = tree;
                }
                return Ok(self.invalid(SwapReport::from_core(rejected).report.errors));
            }
        };
        let Ok(tree_cell) = self.tree_cell() else {
            return Ok(None);
        };
        let unchanged = !self.is_silent()
            && prepared.content_hash() == coordinator.installed_hash()
            && tree_cell.texts == tree.texts;
        drop(tree_cell);
        if unchanged {
            // Valid again with nothing to swap: a subscriber told of the failure hears it ended.
            let Ok(mut reported) = self.reported_invalid.lock() else {
                return Ok(None);
            };
            if reported.take().is_none() {
                return Ok(None);
            }
            let event = Event::Changed {
                hash: coordinator.installed_hash(),
                by: ChangedBy::Outside,
            };
            self.events.publish(event.clone());
            return Ok(Some(event));
        }
        // Refused only with a swap in flight, ruled out above under this same lock.
        if !coordinator.commit_swap(prepared).report.ok {
            return Ok(None);
        }
        if went_live(self, &mut coordinator, Some(top), tree, Went::Outside).is_err() {
            return Ok(None);
        }
        Ok(Some(Event::Changed {
            hash: coordinator.installed_hash(),
            by: ChangedBy::Outside,
        }))
    }

    /// Publish and return [`Event::Invalid`] for `errors`, unless it is the failure last reported.
    /// Called under the Coordinator lock.
    fn invalid(&self, errors: Vec<Diag>) -> Option<Event> {
        let reported = self.reported_invalid.lock().ok()?;
        if reported.as_ref() == Some(&errors) {
            return None;
        }
        drop(reported);
        self.publish_invalid(errors)
    }

    /// Publish and return [`Event::Invalid`] for `errors`, and remember it as the failure last
    /// reported. Called under the Coordinator lock.
    fn publish_invalid(&self, errors: Vec<Diag>) -> Option<Event> {
        *self.reported_invalid.lock().ok()? = Some(errors.clone());
        let event = Event::Invalid { errors };
        self.events.publish(event.clone());
        Some(event)
    }

    /// A receiver of one [`Event`] per install that goes live from now on, in install order. It
    /// disconnects when the state is [closed](Self::close).
    pub fn subscribe(&self) -> Receiver<Event> {
        self.events.subscribe()
    }

    /// [`subscribe`](Self::subscribe) for a connection the host streams events over: dropping it
    /// unsubscribes at once, so a client that leaves while nothing changes is not kept.
    pub fn subscription(&self) -> Subscription {
        self.events.subscription()
    }

    /// How many subscribers events are being sent to.
    pub fn subscribers(&self) -> usize {
        self.events.subscribers()
    }

    /// The canonical sources the last successful install asked its store for, the top document's
    /// included: those it read, and those it referenced and could not read, so a document that is
    /// missing stays one of the instrument's. An edit to any other source is refused.
    pub fn tree(&self) -> BTreeSet<String> {
        self.tree
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .sources
            .clone()
    }

    /// Mark the engine torn down, for every clone of this state: an [`InProcess`] transport over
    /// it then fails each exchange as a dead socket would, instead of answering from a Coordinator
    /// whose render side is gone, and every subscriber's stream disconnects. A host calls it before
    /// it stops its server.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.events.close();
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
            content_hash: self.playing_hash(&coordinator),
            source: source.clone(),
        }
    }

    fn coordinator(&self) -> Result<MutexGuard<'_, Coordinator>, Poisoned> {
        self.coordinator.lock().map_err(|_| Poisoned)
    }

    fn installed_source(&self) -> Result<MutexGuard<'_, Option<String>>, Poisoned> {
        self.installed_source.lock().map_err(|_| Poisoned)
    }

    fn tree_cell(&self) -> Result<MutexGuard<'_, Tree>, Poisoned> {
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
            None => Tree::default(),
        };
        self.tree = Arc::new(Mutex::new(tree));
        self.installed_source = Arc::new(Mutex::new(source));
        self
    }

    /// Start with nothing open and nothing playing, the Coordinator having been built from
    /// [`SILENCE`]: every answer reports the empty content hash, `get_document` no source, and an
    /// edit is refused until an `open` names an instrument.
    ///
    /// A builder step, like [`with_installed_source`](Self::with_installed_source), so the
    /// "only ever written under the Coordinator lock" invariant on the flag has no exception.
    pub fn with_nothing_open(mut self) -> Self {
        self.silent = Arc::new(AtomicBool::new(true));
        self.installed_source = Arc::new(Mutex::new(None));
        self.tree = Arc::new(Mutex::new(Tree::default()));
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

/// [`StructureState::try_reload`]'s answer while an earlier swap is still in flight: nothing was
/// read, and the files should be read again shortly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwapInFlight;

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
        Request::Send { messages } => handle_send(state, messages),
        Request::Call { tool, args } => handle_call(state, &tool, args),
        Request::Open { name, expect } => handle_open(state, name, expect, caller),
        Request::List { what } => handle_list(state, what),
        Request::Library => handle_library(state),
        Request::Surface => handle_surface(state),
        Request::Subscribe => Response::Error {
            message: UNSTREAMED_SUBSCRIBE.to_string(),
        },
    }
}

/// What a `subscribe` answers when it reaches [`dispatch`] rather than a connection that streams.
const UNSTREAMED_SUBSCRIBE: &str = "subscribe needs a connection that streams events; this one \
     answers one response per request";

/// The Coordinator owns the canonical document; serialize it + its hash under the lock so
/// `get_document` never sees a half-installed pair. While the open instrument does not load, the
/// document is `null` and the hash [`NOTHING_PLAYS`]: [`SILENCE`] is no document's.
fn get_document(state: &StructureState) -> Result<Response, Poisoned> {
    let coordinator = state.coordinator()?;
    Ok(Response::Document(DocumentSnapshot {
        content_hash: state.playing_hash(&coordinator),
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

    fn subscribe(
        &self,
    ) -> std::io::Result<Box<dyn Iterator<Item = std::io::Result<Event>> + Send>> {
        Ok(Box::new(self.state.subscribe().into_iter().map(Ok)))
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

/// The install half of [`handle_open`], for `json`, the document read at `origin`, under the
/// Coordinator lock so the `expect`-compare and the swap are one critical section: concurrent opens
/// serialize, and neither `get_document` nor another open sees a half-installed document. In order:
///
/// 1. **Build**: the Coordinator validates and builds a whole new Engine off-thread.
/// 2. **Arbitrate**: a stale `expect` rejects with the real installed hash as [`Response::Conflict`]
///    and does **not** swap. Absent `expect` is last-write-wins. Done here rather than inside the
///    Coordinator's swap, which owns no guard at all, by design.
/// 3. **Hold**: a client's open is offered to [`EngineHost::hold_swap`]; a held one is dropped and
///    answers [`Response::Held`].
/// 4. **Swap**: the build fills the install mailbox. A document that does not load or plan still
///    opens, with nothing playing: see [`open_silent`].
/// 5. **Publish** the render config for the new engine's geometry and fold any warning into the
///    report — BEFORE the reclaim, so the map is in flight before the callback installs the new
///    engine and the two mailboxes stay in lockstep.
/// 6. **Reclaim** the retired Engine off-thread, clearing the mailbox for the next swap.
fn install_opened(
    state: &StructureState,
    coordinator: &mut Coordinator,
    json: &str,
    origin: String,
    expect: Option<String>,
    caller: Caller,
) -> Response {
    // The one-call swap's own first step, kept so a build here peaks at live + new.
    drop(coordinator.try_reclaim());
    let (prepared, tree) = prepare_recording(state, coordinator, json, &origin);

    if let Some(expected) = &expect {
        let actual = state.playing_hash(coordinator);
        if expected != &actual {
            return Response::Conflict(Conflict {
                expected: expected.clone(),
                actual,
            });
        }
    }

    // Offered whether or not the document loads: an open that does not load still unloads what
    // plays.
    if caller == Caller::Client && state.host.hold_swap(&origin) {
        drop(prepared);
        return held(state, coordinator, origin);
    }

    // Unguarded by construction: arbitration was settled above.
    let core_report = match prepared {
        Ok(prepared) => coordinator.commit_swap(prepared),
        Err(rejected) => return open_silent(state, coordinator, origin, tree, rejected),
    };
    let mut report = SwapReport::from_core(core_report);
    if report.report.ok {
        match went_live(state, coordinator, Some(origin), tree, Went::Opened) {
            Ok(warnings) => report.report.warnings.extend(warnings),
            Err(poisoned) => return poisoned.into(),
        }
    } else {
        installed_nothing(state, coordinator, &mut report, Went::Opened);
    }
    Response::SwapReport(report)
}

/// The answer to a client's open the host holds: nothing installed, and what plays named.
fn held(state: &StructureState, coordinator: &Coordinator, requested: String) -> Response {
    let playing = match state.installed_source() {
        Ok(source) => source.clone(),
        Err(poisoned) => return poisoned.into(),
    };
    Response::Held(HeldSwap {
        requested,
        playing,
        content_hash: state.playing_hash(coordinator),
    })
}

/// Open `name`, whose document does not load or plan (`rejected` says why): the instrument that
/// played is unloaded and [`SILENCE`] plays, `name` becomes the open instrument with `tree` as the
/// documents an edit may touch, and [`Event::Opened`] then [`Event::Invalid`] are published.
///
/// The answer is a report with `ok: false`, the load's errors, the hash [`NOTHING_PLAYS`], and a
/// warning that says the document is open. An edit that makes it load starts it playing.
fn open_silent(
    state: &StructureState,
    coordinator: &mut Coordinator,
    name: String,
    tree: Tree,
    rejected: reuben_document::SwapReport,
) -> Response {
    let errors = SwapReport::from_core(rejected).report.errors;
    let silenced = coordinator.swap_document(SILENCE);
    if !silenced.report.ok {
        // Only a swap still in flight refuses a document this fixed.
        let mut report = SwapReport::from_core(silenced);
        installed_nothing(state, coordinator, &mut report, Went::OpenedSilent);
        return Response::SwapReport(report);
    }
    let mut warnings = match went_live(
        state,
        coordinator,
        Some(name.clone()),
        tree,
        Went::OpenedSilent,
    ) {
        Ok(warnings) => warnings,
        Err(poisoned) => return poisoned.into(),
    };
    state.publish_invalid(errors.clone());
    warnings.push(Diag {
        node: None,
        port: None,
        message: format!(
            "{name} is open but does not load, so nothing plays until an edit makes it load"
        ),
    });
    Response::SwapReport(SwapReport {
        report: Report {
            ok: false,
            errors,
            warnings,
        },
        content_hash: NOTHING_PLAYS.to_string(),
        diff: None,
    })
}

/// Unload the open instrument and load, and play, the one `name` names, read through the server's
/// store so the tree and the edits that follow see the same files. A document that does not load
/// still opens, with nothing playing ([`open_silent`]).
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
        return rejected_report(state, &coordinator, not_offered(&name), Went::Opened);
    }
    let json = match state.store.read_text(&origin) {
        Ok(json) => json,
        Err(e) => {
            return rejected_report(
                state,
                &coordinator,
                format!("open {name}: {e}"),
                Went::Opened,
            )
        }
    };
    // By its origin, so the `Opened` every client hears names one spelling.
    install_opened(state, &mut coordinator, &json, origin, expect, caller)
}

/// The refusal for a name outside what the host offers: the client's own spelling, a fixed reason
/// and where the offered names are, so it says nothing of the server's paths or whether the file
/// exists.
fn not_offered(name: &str) -> String {
    format!(
        "`{name}` is not offered by this server; call `list_instruments` to see the names it offers"
    )
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

/// The open instrument's surface document, as the host reads it.
fn handle_surface(state: &StructureState) -> Response {
    let open = match state.installed_source() {
        Ok(source) => source.clone(),
        Err(poisoned) => return poisoned.into(),
    };
    let Some(open) = open else {
        return Response::Surface { text: None };
    };
    match state.host.surface(&open) {
        Ok(text) => Response::Surface { text },
        Err(message) => Response::Error { message },
    }
}

/// The documents an install read, or referenced and could not read.
#[derive(Default)]
struct Tree {
    /// Every canonical source the load asked for, the top document's included, whether or not it
    /// could be read.
    sources: BTreeSet<String>,
    /// The texts of the documents the top one reaches, without the top one's: the installed
    /// content hash already says whether that one changed, and leaving it out keeps a top document
    /// reformatted without a change from swapping.
    texts: BTreeMap<String, u64>,
}

impl Tree {
    /// `reads` from a load of the document at canonical source `top`.
    fn new(mut reads: Reads, top: String) -> Self {
        reads.remove(&top);
        let mut sources: BTreeSet<String> = reads.keys().cloned().collect();
        sources.insert(top);
        Self {
            sources,
            texts: reads,
        }
    }
}

/// Prepare `json`, read from `origin`, and what the load read.
fn prepare_recording(
    state: &StructureState,
    coordinator: &Coordinator,
    json: &str,
    origin: &str,
) -> (
    Result<reuben_document::PreparedSwap, reuben_document::SwapReport>,
    Tree,
) {
    let log = state.store.inner().record_reads();
    let prepared = coordinator.prepare_document_from(json, origin);
    let tree = Tree::new(log.finish(), state.store.canonical(origin, None));
    (prepared, tree)
}

/// Which verb made an install that went live, which decides the [`Event`] it publishes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Went {
    /// `open`: always [`Event::Opened`].
    Opened,
    /// `open` of a document that does not load, with [`SILENCE`] installed in its place:
    /// [`Event::Opened`] with the hash [`NOTHING_PLAYS`].
    OpenedSilent,
    /// An edit through `call`: always [`Event::Changed`] by a client.
    Edited,
    /// A [`StructureState::reload`] after an outside write: [`Event::Changed`] by
    /// [`ChangedBy::Outside`].
    Outside,
}

/// Everything after a committed install, shared by `open`, an edit made through `call` and a reload:
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
    tree: Tree,
    went: Went,
) -> Result<Vec<Diag>, Poisoned> {
    // First, before anything that can fail: the install is already committed, so a poisoned lock
    // below must not leave the hash reported for silence once something else plays.
    state
        .silent
        .store(went == Went::OpenedSilent, Ordering::SeqCst);
    *state.installed_source()? = source.clone();
    *state.tree_cell()? = tree;

    let warnings = state.host.publish_render_config(
        coordinator.installed_channels(),
        coordinator.installed_input_channels(),
    );
    state.host.install_probes(coordinator.installed_probes());

    reclaim_retired(coordinator, state.host.as_ref());

    // What plays changed, so a failure reported against what played before is news again.
    *state.reported_invalid.lock().map_err(|_| Poisoned)? = None;

    let hash = state.playing_hash(coordinator);
    let opened = match went {
        Went::Opened | Went::OpenedSilent => true,
        Went::Edited | Went::Outside => false,
    };
    let by = match went {
        Went::Outside => ChangedBy::Outside,
        Went::Opened | Went::OpenedSilent | Went::Edited => ChangedBy::Client,
    };
    state.events.publish(if opened {
        Event::Opened {
            name: source.unwrap_or_default(),
            hash,
        }
    } else {
        Event::Changed { hash, by }
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

/// A report that installed nothing, for `message`, answering the request `went` names. Called
/// under the Coordinator lock.
fn rejected_report(
    state: &StructureState,
    coordinator: &Coordinator,
    message: String,
    went: Went,
) -> Response {
    let mut report = SwapReport {
        report: Report {
            ok: false,
            errors: vec![Diag {
                node: None,
                port: None,
                message,
            }],
            warnings: Vec::new(),
        },
        content_hash: String::new(),
        diff: None,
    };
    installed_nothing(state, coordinator, &mut report, went);
    Response::SwapReport(report)
}

/// Make `report`, which installed nothing, name what plays: the playing hash and, while nothing
/// plays, a warning naming the instrument still open (or saying none is), so a reader does not take the empty hash
/// for the refused request's own load failing. `went` is the request that installed nothing.
/// Called under the Coordinator lock.
fn installed_nothing(
    state: &StructureState,
    coordinator: &Coordinator,
    report: &mut SwapReport,
    went: Went,
) {
    report.content_hash = state.playing_hash(coordinator);
    if !state.is_silent() {
        return;
    }
    let what = match went {
        Went::Opened | Went::OpenedSilent => "opened",
        Went::Edited | Went::Outside => "installed",
    };
    let message = match state.installed_source().ok().and_then(|open| open.clone()) {
        Some(open) => {
            format!("nothing was {what}; {open} is still open but does not load, so nothing plays")
        }
        None => format!("nothing was {what}; nothing is open, so nothing plays"),
    };
    report.report.warnings.push(Diag {
        node: None,
        port: None,
        message,
    });
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
        fn read_text(&self, source: &str) -> Result<String, ResolveError> {
            if source == PATH {
                return Ok(DOC.to_string());
            }
            Err(ResolveError::NotFound(source.to_string()))
        }
    }

    /// A host that panics under the Coordinator lock — `publish_render_config` runs inside a swap's
    /// critical section — when `panic_on_publish` is set.
    struct Host {
        panic_on_publish: bool,
        /// Holds every client open it is offered, and counts the offers.
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

    /// The one name [`NoStore`] reads [`DOC`] at.
    const PATH: &str = "t.json";

    impl EngineHost for Host {
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
        open_doc(&channel).expect("open");
        assert_eq!(host.installed.load(Ordering::SeqCst), 2);
        let given = host.last_installed.lock().unwrap().clone().expect("given");
        let coordinator = state.coordinator.lock().unwrap();
        assert!(Arc::ptr_eq(&given, &coordinator.installed_probes()));
    }

    fn open_line(expect: Option<String>) -> String {
        Request::Open {
            name: PATH.to_string(),
            expect,
        }
        .to_ndjson()
    }

    #[test]
    fn a_client_open_the_host_holds_installs_nothing_and_names_what_plays() {
        let (state, _host) = holding();
        let before = state.try_installed();
        match dispatch(&state, &open_line(None)) {
            Response::Held(held) => {
                assert_eq!(held.requested, PATH);
                assert_eq!(held.playing.as_deref(), Some("playing.json"));
                let Installed::Known { content_hash, .. } = &before else {
                    panic!("{before:?}");
                };
                assert_eq!(&held.content_hash, content_hash);
            }
            other => panic!("expected a held open, got {other:?}"),
        }
        assert_eq!(state.try_installed(), before);
    }

    #[test]
    fn the_hosts_own_open_is_never_offered_for_holding() {
        let (state, host) = holding();
        let channel = Channel::new(InProcess::new(state.clone()));
        match open_doc(&channel) {
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
        match dispatch(&state, &open_line(Some("0badc0de".to_string()))) {
            Response::Conflict(conflict) => assert_eq!(conflict.expected, "0badc0de"),
            other => panic!("expected a conflict, got {other:?}"),
        }
        assert_eq!(host.offered.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn an_unreadable_source_is_rejected_before_the_host_is_asked_to_hold() {
        let (state, host) = holding();
        let line = Request::Open {
            name: "missing.json".to_string(),
            expect: None,
        }
        .to_ndjson();
        match dispatch(&state, &line) {
            Response::SwapReport(report) => assert!(!report.report.ok),
            other => panic!("expected a rejected report, got {other:?}"),
        }
        assert_eq!(host.offered.load(Ordering::SeqCst), 0);
    }

    fn open_doc(channel: &Channel) -> Result<super::super::SwapOutcome, ChannelError> {
        channel.open(PATH.to_string(), None)
    }

    #[test]
    fn a_panic_under_the_lock_is_unreachable_then_every_verb_refuses_without_panicking() {
        let state = state(true);
        let channel = Channel::new(InProcess::new(state.clone()));

        let first = open_doc(&channel).expect_err("the panicking open has no answer");
        assert!(first.is_unreachable(), "{first}");

        // Poisoned now: in-process and over any other door, an error rather than a panic.
        for err in [
            open_doc(&channel).expect_err("poisoned"),
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
    fn close_disconnects_in_process_subscribers() {
        let state = state(false);
        let events = InProcess::new(state.clone()).subscribe();
        state.close();
        assert_eq!(
            events.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        );
        assert_eq!(state.subscribers(), 0);
    }

    /// `get_document` names what plays and its hash, and never hands a client the document.
    #[test]
    fn a_snapshot_carries_no_document() {
        let fields = |state: &StructureState| {
            let line = dispatch(state, &Request::GetDocument.to_ndjson()).to_ndjson();
            let value: serde_json::Value = serde_json::from_str(&line).expect("a JSON object");
            let mut keys: Vec<String> = value
                .as_object()
                .expect("an object")
                .keys()
                .cloned()
                .collect();
            keys.sort();
            (keys, value)
        };
        let (keys, _) = fields(&state(false));
        assert_eq!(keys, ["content_hash", "reply", "source"]);
        let (keys, value) = fields(&state(false).with_nothing_open());
        assert_eq!(keys, ["content_hash", "reply"]);
        assert_eq!(value["content_hash"], "");
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
        assert!(open_doc(&channel).expect_err("closed").is_unreachable());
    }
}
