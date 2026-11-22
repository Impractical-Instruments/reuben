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
     These values live in render state only, never in the instrument document. A value sent to \
     an interface input (/<name>/in) lasts through edits, clamped into a new range, until an \
     edit changes its type or default or renames or removes it; one sent to a node input \
     nothing feeds lasts while the node survives and its document value is unchanged. A message \
     to an input a wire or an interface input feeds is dropped and counted \
     (wired_writes_dropped); send to the interface input instead. Opening an instrument, even \
     the one playing, replaces them all. Fold any you want to keep into the instrument \
     document with an edit. The ack means the engine received \
     the batch and queued it for the next rendered block — not that it took effect: an address \
     matching no node/port is dropped silently, exactly as it would be arriving from an external \
     controller. \
     Fails fast if no engine is reachable.";

pub const GET_ENGINE_STATUS: &str =
    "Report whether the reuben engine is reachable, with the engine endpoint and the \
     sidecar version + supported instrument format_version. Never an error — a dead engine is \
     reported as reachable:false with guidance to start it.";

pub const GET_CURRENT_INSTRUMENT: &str =
    "Report what the engine is playing: the source of the open instrument, its content hash, \
     and the node index of that instrument. An empty content hash means nothing plays: with a \
     source, the open instrument does not load until an edit makes it load; with none, no \
     instrument is open, so open one. Fails fast if no engine is reachable.";

pub const GET_ENGINE_DIAGNOSTICS: &str =
    "Return the engine's running diagnostics counters since start: output_xruns (events), \
     input_ring underruns/overruns/producer_drops (frames), control_batches_dropped_full \
     (batches), and wired_writes_dropped (control messages dropped for addressing a node input \
     a wire or an interface input feeds). Fails fast if no engine is reachable.";

pub const GET_ENGINE_METERS: &str =
    "Return the engine's levels at the audio device, one entry per output and input channel, as \
     peak_dbfs and rms_dbfs. Use it to check that an instrument makes sound (an output above the \
     -120 floor), is clipping (peak at or above 0), or hears its input. Peaks fall back at 12 dB \
     a second and RMS averages over 0.3 s, so call it while the sound plays, not after. Fails \
     fast if no engine is reachable.";

pub const OPEN_INSTRUMENT: &str =
    "Stop playing the current instrument and load and play the one named `name`, a name \
     `list_instruments` gave. Edits apply only to the open instrument and its nested files, so \
     open an instrument before editing it. One that does not load still opens, with its errors \
     and an empty content_hash: nothing plays until an edit makes it load.";

pub const LIST_INSTRUMENTS: &str =
    "List the names the engine offers: `what: \"instruments\"` for what `open_instrument` accepts \
     and a `source` may name, `\"samples\"` for what a sample resource may reference.";
