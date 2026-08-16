//! The one sentence each engine verb is advertised by, and the one message every engine verb
//! returns when there is no engine to reach.
//!
//! Same rule as the authoring half's prose: every string here ships to a model, so no rustdoc link
//! syntax, no issue numbers, no crate paths. Notes for humans go in `//` comments. Guarded over the
//! real advertised surface by `advertised_prose_is_model_facing`.

/// What an engine verb says when the engine is not there. One string, because the fix is one
/// action and a per-verb wording would be a per-verb opinion about whose fault it is.
pub const ENGINE_UNREACHABLE_GUIDANCE: &str =
    "The reuben engine is not reachable. Start it in another terminal with `reuben play`, then retry.";

pub const SEND_LIVE_CONTROLS: &str =
    "Send a batch of control messages to audition a change on the running engine. \
     Ephemeral by design: these values live in render state only and are \
     CLOBBERED at the next swap — fold any you want to keep into the instrument document \
     and swap. The ack means the engine received the batch and queued it for the next \
     rendered block — not that it took effect: an address matching no node/port is \
     dropped silently, exactly as it would be arriving from an external controller. \
     Fails fast if no engine is reachable.";

pub const GET_ENGINE_STATUS: &str =
    "Report whether the reuben engine is reachable, with the structure endpoint and the \
     sidecar version + supported instrument format_version. Never an error — a dead engine is \
     reported as reachable:false with guidance to start it.";

pub const SWAP_INSTRUMENT: &str =
    "Install an instrument document from disk as the playing engine (path-only). A gapless \
     mailbox swap: the new Engine is built and validated off-thread, then \
     installed under a ~20ms master-gain duck — no silent gap. A node at the same \
     address with the same operator type survives with its live state, so the diff summary \
     reports how many survived and which reset. Returns the validation report + content_hash \
     + (on success) that diff summary; ok:false installs nothing and the old sound keeps \
     playing. Pass `expect` (a content_hash) to guard against a stale swap — a mismatch \
     returns a conflict, no install.";

pub const GET_CURRENT_INSTRUMENT: &str =
    "Report what the engine is playing: the source it was installed from, the installed content \
     hash, and the node index of the live graph. Compare the hash against the one a document \
     verb returns for that source to see whether your edits have been swapped in yet. Fails \
     fast if no engine is reachable.";

pub const GET_ENGINE_DIAGNOSTICS: &str =
    "Return the engine's running diagnostics counters since start: output_xruns (events) plus \
     input_ring underruns/overruns/producer_drops (frames). Fails fast if no engine is reachable.";
