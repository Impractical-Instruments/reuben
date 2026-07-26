//! The serving side of the structure channel: what each verb *means*, over the seam a host fills.
//!
//! One [`Response`] per [`Request`], in order. There is no socket here and no thread — a host owns
//! the listener, the framing loop and the shutdown; this is the part that would otherwise be
//! rewritten per host, and rewritten differently.
//!
//! The five verbs:
//! - [`Request::Ping`] → [`Response::Pong`] — liveness of the channel itself.
//! - [`Request::GetDocument`] → the Coordinator's canonical document + its content hash. It changes
//!   only when a [`Request::Swap`] installs a new one.
//! - [`Request::GetDiagnostics`] → the host's counters, snapshotted off-thread.
//! - [`Request::Swap`] → a **mailbox swap**: the Coordinator validates and builds a whole new
//!   Engine off-thread, fills the install mailbox, and returns a real report with survivor/reset
//!   stats. The `expect` guard is applied here, inside the Coordinator lock, so the compare and the
//!   swap are one critical section.
//! - [`Request::Send`] → control traffic, handed to the host's ingress as **one** batch. The one
//!   verb that does not touch the Coordinator: control is not structure.
//!
//! The Coordinator is single-writer, so it sits behind one [`Mutex`] here and concurrent
//! connections serialize on it.
//!
//! see rules: execution-runtime

use std::sync::{Arc, Mutex};

use reuben_core::coordinator::Coordinator;

use crate::authoring::{Diag, Report};

use super::wire::{
    over_long_batch_refusal, Conflict, ControlMessage, DiagnosticsReport, DocSource,
    DocumentSnapshot, Request, Response, SwapReport, EMPTY_BATCH_REFUSAL, MAX_SEND_BATCH,
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

    /// Hand a control batch to the engine's ingress **as one unit**, so a gesture cannot straddle
    /// a rendered block. `Err` means the ingress is gone (audio torn down) and nothing was queued.
    fn deliver_control(&self, messages: Vec<ControlMessage>) -> Result<(), IngressClosed>;

    /// The running counters, as an owned copy taken off the audio thread.
    fn diagnostics(&self) -> DiagnosticsReport;

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
}

impl StructureState {
    /// Wrap a Coordinator the host built (with `install_initial`) and the seam it serves through.
    pub fn new(coordinator: Coordinator, host: Arc<dyn EngineHost>) -> Self {
        Self {
            coordinator: Arc::new(Mutex::new(coordinator)),
            host,
            installed_source: Arc::new(Mutex::new(None)),
        }
    }

    /// Name the source the *initial* document was installed from. A builder step because it has a
    /// working default (`None`, a built-in rig): every later swap re-points it from the `source` it
    /// installed, so this only ever answers for the run's first document.
    ///
    /// **Replaces the cell rather than writing through it**, so the "only ever written under the
    /// Coordinator lock" invariant on that field has no exception to carve out. Taking `self` by
    /// value makes it structurally true: a builder step can only run before there is anyone to race.
    pub fn with_installed_source(mut self, source: Option<String>) -> Self {
        self.installed_source = Arc::new(Mutex::new(source));
        self
    }
}

/// Dispatch one request line to its response. Pure over [`StructureState`], so a host can drive it
/// without a socket and the framing loop stays the host's.
///
/// An unreadable line is a channel-level [`Response::Error`] (distinct from a domain answer that
/// reports failure), so a malformed request still gets exactly one framed reply and the
/// one-response-per-request invariant holds.
pub fn dispatch(state: &StructureState, line: &str) -> Response {
    match Request::from_ndjson(line) {
        Ok(Request::Ping) => Response::Pong,
        Ok(Request::GetDocument) => {
            // The Coordinator owns the canonical document; serialize it + its hash under the lock
            // so `get_document` never sees a half-installed pair.
            let coordinator = state
                .coordinator
                .lock()
                .expect("coordinator mutex poisoned");
            let document = serde_json::to_value(&**coordinator.document())
                .expect("canonical instrument document serializes to JSON");
            Response::Document(DocumentSnapshot {
                document,
                content_hash: coordinator.installed_hash(),
                // Read under the Coordinator lock too, so the source and the hash a caller compares
                // it against can never come from different installs.
                source: state
                    .installed_source
                    .lock()
                    .expect("installed-source mutex poisoned")
                    .clone(),
            })
        }
        Ok(Request::GetDiagnostics) => Response::Diagnostics(state.host.diagnostics()),
        Ok(Request::Swap { source, expect }) => handle_swap(state, source, expect),
        Ok(Request::Send { messages }) => handle_send(state, messages),
        Err(e) => Response::Error {
            message: format!("unreadable request: {e}"),
        },
    }
}

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

/// The mailbox-swap install path. Everything runs under the Coordinator lock so the
/// `expect`-compare and the swap are one atomic critical section (a compare-and-swap) — concurrent
/// swaps from multiple connections serialize, and neither `get_document` nor another swap sees a
/// half-installed document. In order:
///
/// 1. **Resolve** the source to its JSON text — inline JSON re-serialized, or a read through the
///    host. A read failure is a rejected report (no install, prior retained), not a channel error.
/// 2. **Arbitrate**: a stale `expect` rejects with the real installed hash as [`Response::Conflict`]
///    and does **not** swap. Absent `expect` is last-write-wins. Done here rather than inside the
///    Coordinator's swap, which owns no guard at all, by design (see rules: agent-mcp).
/// 3. **Swap**: the Coordinator validates + builds a whole new Engine off-thread, fills the install
///    mailbox, and returns the real report. A load/plan error aborts with `ok: false` and the prior
///    hash — the old engine keeps playing (retain-prior).
/// 4. **Publish** the render config for the new engine's geometry and fold any warning into the
///    report — BEFORE the reclaim, so the map is in flight before the callback installs the new
///    engine and the two mailboxes stay in lockstep.
/// 5. **Reclaim** the retired Engine off-thread, clearing the mailbox for the next swap.
fn handle_swap(state: &StructureState, source: DocSource, expect: Option<String>) -> Response {
    // Name the source before resolving consumes it — recorded only if the install below succeeds, so
    // a rejected swap leaves `get_document` still naming what is actually playing. An install by
    // value has no source to name.
    let installed_from = match &source {
        DocSource::Path(path) => Some(path.clone()),
        DocSource::Document(_) => None,
    };

    let json = match resolve_source(source, state.host.as_ref()) {
        Ok(json) => json,
        Err(message) => return rejected_swap(&state.coordinator, message),
    };

    let mut coordinator = state
        .coordinator
        .lock()
        .expect("coordinator mutex poisoned");

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
    let mut report = SwapReport::from_core(coordinator.swap_document(&json));
    if report.report.ok {
        // The installed document advanced, so the source that named it does too — under the
        // Coordinator lock, so the two never disagree about which install a reader is looking at.
        *state
            .installed_source
            .lock()
            .expect("installed-source mutex poisoned") = installed_from;

        let logical = coordinator.installed_channels();
        let input_channels = coordinator.installed_input_channels();
        report
            .report
            .warnings
            .extend(state.host.publish_render_config(logical, input_channels));

        reclaim_retired(&mut coordinator, state.host.as_ref());
    }
    Response::SwapReport(report)
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
fn rejected_swap(coordinator: &Arc<Mutex<Coordinator>>, message: String) -> Response {
    let content_hash = coordinator
        .lock()
        .expect("coordinator mutex poisoned")
        .installed_hash();
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
