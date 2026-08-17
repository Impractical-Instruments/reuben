//! What the engine verbs answer with — the window's own result shapes, over the channel payloads
//! in [`wire`](super::wire).
//!
//! The same rule the authoring results carry applies here: every field `///` is advertised prose,
//! and so is the type `///` of anything that lands in `$defs` ([`StatusEndpoints`],
//! [`SidecarInfo`]). Write them as if they ship, because they do — no rustdoc markup, no issue
//! numbers, no crate paths.

use alloc::string::String;

use serde::{Deserialize, Serialize};

use super::wire::{Conflict, SwapReport};

/// Output for `send`: how many messages were queued.
///
/// The engine acks the batch as a unit, so this is the batch's own size — there is no partial
/// outcome to report, which is why the wire's ack carries no count of its own. "Queued for the next
/// block", not "applied": an address matching no node/port is dropped at the engine's ingress,
/// exactly as an external OSC datagram naming a stale address is.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SendOutput {
    /// The number of control messages the engine queued.
    pub sent: usize,
}

/// The endpoint `engine_status` reports. One field, because the sidecar now talks to the engine
/// over exactly one channel; the engine's OSC-in port is its foreign edge, which this sidecar
/// neither dials nor owns (`reuben play` prints it at startup for whoever points a controller at
/// it).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StatusEndpoints {
    /// The structure channel address — every engine tool speaks it
    /// (`ping`/`send`/`swap`/`get_document`/`get_diagnostics`).
    pub structure: String,
}

/// The sidecar identity `engine_status` reports: its own version and the instrument
/// `format_version` it supports (kept here, out of per-call reports).
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SidecarInfo {
    /// The reuben-mcp crate version.
    pub version: String,
    /// The instrument document `format_version` this sidecar loads.
    pub format_version: u32,
}

/// Output for `engine_status`. **Never an error** for a dead engine — `reachable` and the
/// `guidance` (present only when unreachable) ARE the deliverable.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EngineStatus {
    /// Whether a live `reuben play` answered `ping` on the structure channel.
    pub reachable: bool,
    /// The endpoint this sidecar talks to.
    pub endpoints: StatusEndpoints,
    /// The sidecar's own identity.
    pub sidecar: SidecarInfo,
    /// The "start `reuben play`" guidance — present only when the engine is unreachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

/// Output for `swap`: the shared swap report (ok, errors, warnings, content_hash, and on success
/// the diff summary) plus, on an `expect`-guard miss, `conflict`.
///
// One `outputSchema` spans the install, validation-failure and guard-miss cases; both the flattened
// `SwapReport` and the `Conflict` are the same types the structure channel serializes, so the tool
// shape and the wire shape cannot drift.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SwapResult {
    /// The install report: `ok`, `errors`, `warnings`, the installed (or still-playing) content
    /// hash, and — on a successful install only — the diff summary.
    #[serde(flatten)]
    pub report: SwapReport,
    /// Present only on an `expect`-guard miss: nothing was installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<Conflict>,
}

impl SwapResult {
    /// The engine processed the swap (success or `ok: false` load failure): the report is the whole
    /// story, no conflict.
    pub fn installed(report: SwapReport) -> Self {
        Self {
            report,
            conflict: None,
        }
    }

    /// The `expect` guard missed: nothing installed. The report is the rejected one — which owns
    /// the "`content_hash` names what keeps playing" contract — and the channel's own conflict
    /// rides along verbatim for the model to reconcile against.
    pub fn conflict(conflict: Conflict) -> Self {
        Self {
            report: SwapReport::rejected(conflict.actual.clone()),
            conflict: Some(conflict),
        }
    }
}

/// Output for `get_current_instrument`: what the engine is playing, without the document.
///
// The tool's question — "what is the engine playing?" — is not answerable by `{ source, hash }`
// alone: `swap` installs *from* a source, and the agent may have edited that source since, so
// re-reading it would answer a different question. The projection here is cut from what is actually
// installed — the document the channel handed back, which reaches a door and stops there.
//
// The drift signal then comes free: compare `content_hash` against the hash any document verb
// returns for `source`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CurrentInstrument {
    /// Where the playing document was installed from, when the engine knows it. Absent after an
    /// install by value and for `reuben play`'s built-in default rig.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The installed document's content hash — the token a later `swap_instrument`'s `expect` guard
    /// compares, and the drift signal against `source`'s current hash.
    pub content_hash: String,
    /// The node index of the **installed** graph, rendered in the projection's line grammar.
    pub projection: String,
}
