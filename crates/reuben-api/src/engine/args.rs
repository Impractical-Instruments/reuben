//! The engine verbs' argument surface: flat, like the authoring half's, and advertised the same
//! way — every `///` here is the field `description` a model reads.
//!
//! Three of the five verbs take nothing at all (`get_engine_status`, `get_current_instrument`,
//! `get_engine_diagnostics` each ask the one question their name asks), so only two structs live
//! here. see rules: agent-mcp

use serde::{Deserialize, Serialize};

/// One control message in a `send` batch: an address and its primitive args.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ControlSendMessage {
    /// The address, e.g. `/voice1/cutoff`.
    pub address: String,
    /// The arguments — numbers or strings.
    #[serde(default)]
    #[schemars(with = "Vec<crate::schema::Literal>")]
    pub args: Vec<serde_json::Value>,
}

/// Arguments for `send_live_controls`: a batch of control messages (the natural authoring gesture
/// is multi-control), bounded at both ends.
///
// The `length` bounds put `minItems`/`maxItems` in the advertised input schema. The upper bound is
// `MAX_SEND_BATCH` and is an RT limit, not a request-size one: the engine applies a batch to a
// single render callback, so an unbounded one could blow a deadline. Advertising it is a courtesy
// that lets a model split its own gesture; the verb enforces it regardless, for a client that skips
// schema validation.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SendLiveControls {
    /// The control messages to apply, in order.
    // The literal must equal MAX_SEND_BATCH; schemars takes a literal, not a const. Pinned against
    // the real advertised schema by reuben-mcp's
    // `send_live_controls_schema_advertises_the_shared_batch_limit`.
    #[schemars(length(min = 1, max = 256))]
    pub messages: Vec<ControlSendMessage>,
}

/// Arguments for `swap_instrument`: a `path` (path-only — you can only install what exists on
/// disk) plus an optional `expect` content-hash guard.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SwapInstrument {
    /// Path to the instrument document to install.
    pub path: String,
    /// The content hash the client believes is installed; a mismatch rejects the swap.
    #[serde(default)]
    pub expect: Option<String>,
}
