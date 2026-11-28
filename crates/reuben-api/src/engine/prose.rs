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
     nothing feeds lasts while the node survives and its document value is unchanged. Opening an \
     instrument, even the one playing, replaces them all. Fold any you want to keep into the \
     instrument document with an edit. The whole batch is refused, naming the first message at \
     fault and its address, and nothing in it is applied, when a message's address reaches no \
     input, its args don't fit the input, or a wire or an interface input feeds the input (send \
     to the interface input instead). A message to a str input must be one string, compiled \
     first: anything else, or text that does not compile, refuses the batch the same way, with \
     the bytes at fault. An accepted batch is queued for the next rendered block. \
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
     (batches), control_messages_dropped_full (messages dropped for arriving with the engine's \
     per-block queue full: only one OSC bundle larger than the queue does this), and \
     wired_writes_dropped (control messages dropped for addressing an input a \
     wire or an interface input feeds: a node input, or a subpatch boundary input such as \
     /a/level/in that the host feeds). \
     retire_queue_dropped_full (values): replaced strings the audio thread freed itself because \
     the queue that frees them elsewhere was full; nothing is lost, but each one risks a click, \
     so a rising count means strings are changing faster than they are cleared. \
     unprepared_writes_dropped (messages): strings dropped at a str input because they were not \
     compiled for it, usually raw text arriving over OSC; the input keeps its old value, so send \
     the text with send_live_controls instead. \
     unaddressed_messages_dropped (messages): control dropped because its address reaches no \
     input of the playing instrument, a typo over OSC or an address an edit removed after \
     send_live_controls checked it; unfit_args_dropped (messages): control dropped because its \
     args do not fit the input it addresses. \
     stale_sends_dropped (messages): send_live_controls messages in batches carrying compiled \
     strings that reached the engine after an edit or open had replaced what they were compiled \
     for, dropped whole; resend the batch. \
     pattern_queries_truncated (queries): pattern cycles that had more notes than a pattern node \
     can hold, so some of that cycle's notes never played; thin the pattern. \
     pattern_atoms_skipped (atoms): pattern steps the node's mode cannot read (a note name outside \
     Midi mode, a degree that is not a whole number), which stay silent; fix the text or the \
     mode. \
     pattern_notes_evicted (notes): sounding pattern notes cut short because too many were held \
     at once; shorten the notes or thin the overlap. \
     midi_received (messages): MIDI messages read from a device or injected by a client, \
     unsupported ones included and clock, active sensing and SysEx excepted; a throughput count, not a fault, expected to rise \
     while MIDI is played, and zero while playing means the engine hears no MIDI input, so check \
     the device. \
     midi_unsupported (messages): received MIDI messages of a kind no MIDI map can bind \
     (polyphonic aftertouch) or bytes that are not one well-formed message, dropped; set the \
     controller to send channel pressure instead of polyphonic aftertouch. \
     Fails fast if no engine is reachable.";

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
