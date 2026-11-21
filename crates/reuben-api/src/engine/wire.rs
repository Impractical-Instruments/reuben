//! The structure channel's NDJSON wire envelope: the `Request`/`Response` types a server and a
//! client both serialize, one JSON object per line, one response per request in order.
//!
//! No engine, no threads, no sockets. Both ends are doors — `reuben play` serves it, the MCP
//! sidecar dials it — so the envelope is the window's, and neither door can move it alone.
//!
//! Every payload here is declared rather than borrowed from the engine, [`SwapReport`] included:
//! the swap's report is the flat object a model reads out of `open_instrument`, and the engine's own
//! is the shape its Coordinator finds convenient. The two are converted at
//! [`SwapReport::from_core`], once per swap and never on a block.
//!
//! Framing is newline-delimited JSON
//! ([`Request::to_ndjson`]/[`Request::from_ndjson`] and the `Response` pair) so the channel
//! stays netcat-debuggable and std-only.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use std::io::{self, BufRead, Read};

use serde::{Deserialize, Serialize};

use reuben_core::message::Arg;

use crate::authoring::{Diag, Report};

/// The structure channel's default loopback bind/target: `127.0.0.1` only, shared here so the
/// reuben-native server (`reuben play`) and the reuben-mcp client bind and dial the *same*
/// address. A taken port is non-fatal on the server side (see `play`).
pub const DEFAULT_STRUCTURE_ADDR: &str = "127.0.0.1:9124";

/// The most paths one [`Request::SetProbes`] may name. It bounds paths, not per-block work: Render
/// folds a probed node's output once per block, and a path inside a voice once per voice per
/// block, so the choice is bounded for the same reason a `send` batch is.
pub const MAX_PROBES: usize = 512;

/// The most messages one [`Request::Send`] may carry.
///
/// A batch is delivered to the render callback as **one unit**, so its whole cost lands in a single
/// audio callback: one `queue_osc` route-and-push per message, onto the `pending` vector the engine
/// drains each block. That is cheap per message and must stay bounded — an unbounded batch would let
/// one authoring gesture blow a render deadline, and "Render is RT-safe" is not negotiable. UDP got
/// this bound for free from the kernel receive buffer; this channel has to state it.
///
/// 256 is generous against the real gestures: an authoring move is a handful of controls, and
/// recalling a whole surface is on the order of a hundred. A client with more to say sends another
/// batch. Shared here so the door advertising a `maxItems` and the engine enforcing it cannot drift.
pub const MAX_SEND_BATCH: usize = 256;

/// Why an empty batch is refused. Stated once because both ends check it — the client so a bad
/// batch never reaches the wire, the server because it cannot trust that a client did — and a
/// caller reading two wordings for one condition has to work out whether they mean the same thing.
pub const EMPTY_BATCH_REFUSAL: &str = "`send` needs at least one message; an empty batch does \
                                       nothing, and acking it as success would let a client that \
                                       drops its messages read as a working send.";

/// Why a batch over [`MAX_SEND_BATCH`] is refused, given how many it carried. The counterpart to
/// [`EMPTY_BATCH_REFUSAL`]: one sentence for the condition, both ends.
pub fn over_long_batch_refusal(len: usize) -> String {
    format!(
        "`send` takes at most {MAX_SEND_BATCH} messages ({len} given); split the gesture across \
         several sends."
    )
}

/// Why a probe set over [`MAX_PROBES`] is refused: one sentence for the condition, both ends.
pub fn over_long_probe_set_refusal() -> String {
    format!("at most {MAX_PROBES} probes")
}

/// One control atom on this channel: the **flat primitive form** — a number or a string, and
/// nothing else.
///
/// This is deliberately *not* [`Arg`], the central engine enum. `Arg` also carries `Note`,
/// `Harmony`, `Pitch`, `Enum`, and a whole `F32Buffer` — a vocabulary a control wire would
/// immediately have to forbid. Every door ships `{address, [Arg]}` in its **own local framing**
/// (reuben-web hand-rolls a flat codec, `reuben play`'s foreign edge speaks OSC-the-binary-protocol);
/// this is the structure channel's, and each converges at
/// [`Engine::queue_osc`](reuben_core::engine::Engine::queue_osc), where the destination port's declared
/// type drives the conversion to the single typed `Arg` it carries
/// ([`osc_in_arg`](reuben_core::boundary::osc_in_arg)). So the primitives are all this type needs to
/// spell.
///
/// **Untagged**, so an atom rides the wire as its bare JSON value — `[800.0, "up", 3]`, not
/// `[{"f32": 800.0}, …]` — which keeps the channel netcat-debuggable (see the module header) and
/// makes the `I32`/`F32` split fall out of JSON's own integer-vs-float spelling rather than a rule
/// each client reimplements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ControlArg {
    // VARIANT ORDER IS LOAD-BEARING. Untagged deserialization tries variants top-down and takes the
    // first that fits, so `I32` must precede `F32`: with `F32` first, the JSON `3` would parse as
    // `F32(3.0)` and every integer atom would silently arrive as a float. `800.0` still reaches
    // `F32` because `deserialize_i32` rejects a float-backed number — including the integral-valued
    // `69.0` of a MIDI note, which serde_json emits with its `.0` intact. Pinned by
    // `control_args_split_integers_from_floats`.
    /// An integer atom (a JSON integer within `i32` range).
    I32(i32),
    /// A number atom (any other JSON number).
    F32(f32),
    /// A string / symbol atom — an enum variant name, typically.
    Str(String),
}

impl From<ControlArg> for Arg {
    fn from(value: ControlArg) -> Self {
        match value {
            ControlArg::I32(v) => Arg::I32(v),
            ControlArg::F32(v) => Arg::F32(v),
            ControlArg::Str(s) => Arg::Str(s.as_str().into()),
        }
    }
}

/// One control message in a [`Request::Send`] batch: an address plus its flat atoms — the
/// `{address, [Arg]}` pair every door ships.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlMessage {
    /// The full address, e.g. `/filt/cutoff`. Routed by the engine exactly as an address arriving
    /// on the foreign OSC edge is; an address matching no node/port is dropped silently.
    pub address: String,
    /// The message's atoms, in order.
    #[serde(default)]
    pub args: Vec<ControlArg>,
}

/// One structure-channel request (its twelve verbs), serialized as a single JSON
/// object tagged by `verb`: `{"verb": "ping"}`, `{"verb": "open", "name": …}`, ….
///
/// No request carries a document: a client changes what plays only by
/// [`Open`](Self::Open) or an edit through [`Call`](Self::Call).
///
/// Four of them make the engine the server of a client's authoring: [`Call`](Self::Call) edits
/// and reads the open instrument, [`Open`](Self::Open) plays another, [`List`](Self::List) says
/// what can be named, and [`Subscribe`](Self::Subscribe) follows what changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum Request {
    /// Liveness — the structure channel's own probe: it proves the structure channel
    /// itself, which an OSC ping does not.
    Ping,
    /// Read the currently installed document — the Coordinator owns the canonical doc,
    /// so a fresh conversation attaches in one call.
    GetDocument,
    /// Read the diagnostics counters, exposed past log-only (this channel is their
    /// vehicle).
    GetDiagnostics,
    /// Read the engine's input and output levels, for a client to draw meters from.
    GetMeters,
    /// Choose which nodes' output levels [`GetMeters`](Self::GetMeters) reports, replacing the
    /// previous choice. Each path is a node's address from the root: one name per nesting level,
    /// so `["/voice", "/osc"]` is `/osc` inside the voice patch the node `/voice` hosts. A subpatch's
    /// spliced child is one element: `"/bass/filter"`.
    ///
    /// The engine measures only the chosen nodes, so an empty list turns node metering off. A path
    /// naming no node, or a node with no Signal output, is left out of the answer; the others are
    /// still metered. At most [`MAX_PROBES`] paths. The choice applies to the installed
    /// instrument and carries across a swap for every path the new instrument still has.
    SetProbes {
        /// The node paths to meter.
        paths: Vec<Vec<String>>,
    },
    /// Audition control values on the running engine — control traffic, riding the same channel as
    /// the structure verbs.
    ///
    /// Batched because the authoring gesture is multi-control: one exchange delivers the batch, and
    /// the engine hands it to its ingress as a **single unit**, so the whole gesture reaches the
    /// same rendered block and no concurrent client interleaves into the middle of it. From there
    /// it takes the same path a decoded external OSC datagram does, so routing, typing, and
    /// block-quantized timing are identical — this channel carries no wire format of its own past
    /// [`ControlArg`].
    ///
    /// Between 1 and [`MAX_SEND_BATCH`] messages; the engine rejects anything outside that with
    /// [`Response::Error`], so an empty batch is a client bug reported rather than a no-op acked.
    Send {
        /// The messages to apply, in order.
        messages: Vec<ControlMessage>,
    },
    /// Run one non-engine roster verb against the engine's store. A read answers directly. An edit
    /// is written only if the open instrument, with the edit, still loads and plans, and then the
    /// new Plan is swapped in; otherwise nothing is written and the answer says why.
    Call {
        /// The roster verb's name.
        tool: String,
        /// The verb's arguments, as its argument struct deserializes them.
        args: serde_json::Value,
    },
    /// Unload the open instrument and load, and play, the one `name` names, read through the
    /// engine's store. The one way a client changes which document plays. Answers with a
    /// [`Response::SwapReport`], [`Response::Held`] or [`Response::Conflict`].
    Open {
        /// One of the names [`List`](Self::List) offers.
        name: String,
        /// The opt-in concurrency guard: the [`content_hash`](reuben_document::content_hash) the
        /// client believes plays. A mismatch answers [`Response::Conflict`] and installs nothing.
        /// `None` is last-write-wins.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<String>,
    },
    /// Ask what instruments or samples the engine offers.
    List {
        /// Which kind of name to list.
        what: ListWhat,
    },
    /// Ask for the start screen's whole view of the library: every instrument the engine offers,
    /// read, and the directories a new one may be created in.
    Library,
    /// Ask for the open instrument's surface document: the text of a JSON file beside the
    /// instrument that is not itself an instrument document.
    Surface,
    /// Turn this connection into a stream of [`Event`]s: answered with [`Response::Subscribed`],
    /// then one event per line, one per change, for as long as the connection stays open. The
    /// connection answers no further requests.
    Subscribe,
}

/// Which kind of name to list.
//
// The wire's `Request::List` and `list_instruments` share this type, so its docs ship to models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ListWhat {
    /// The instruments `open_instrument` accepts.
    Instruments,
    /// The samples a sample resource may reference.
    Samples,
}

/// `Library`'s answer: what a start screen draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Library {
    /// One entry per instrument the engine offers, in the order it offers them.
    pub instruments: Vec<LibraryEntry>,
    /// The directories a new instrument may be created in, each a prefix a name can be joined to.
    pub dirs: Vec<String>,
}

/// One instrument of the [`Library`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryEntry {
    /// The name to [`Open`](Request::Open) it by.
    pub name: String,
    /// The document's `instrument` name; absent when it could not be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Why the document could not be read, when it could not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Every canonical source the document's tree reads, itself included: what a client matches to
    /// say which instruments use a document.
    #[serde(default)]
    pub reads: Vec<String>,
}

impl Request {
    /// Serialize as one newline-terminated JSON line — the NDJSON framing both ends write.
    pub fn to_ndjson(&self) -> String {
        to_ndjson_line(self)
    }

    /// Parse one NDJSON line (trailing newline tolerated, as `serde_json` skips trailing
    /// whitespace). Errors are the caller's to wrap — the envelope does not police I/O.
    pub fn from_ndjson(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// An `expect`-guard miss: the request was rejected and **nothing was installed** — the engine is
/// still playing what it was playing before. Re-read the installed document, then retry against
/// the hash that is actually installed.
///
// Everything above ships to models — it becomes the `$defs.Conflict` description in
// `open_instrument`'s advertised outputSchema — so it stays about what the model should DO. Notes
// for humans go below this line, where schemars will not pick them up.
//
// One type, three doors: `Response::Conflict`, the channel client's `SwapOutcome::Conflict`, and
// `open_instrument`'s `conflict` field all carry this struct, so the shapes cannot drift.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Conflict {
    /// The content hash the client asserted was installed.
    pub expected: String,
    /// The content hash actually still playing — re-read with `get_current_instrument` to
    /// reconcile, then retry with this as `expect`.
    pub actual: String,
}

/// The open was held, not installed: the engine is embedded in reuben-editor, whose user has a
/// different file open, and they have been asked whether to open this one instead. Nothing changed and
/// `playing` keeps playing. Not a failure: retrying asks them again. Check
/// `get_current_instrument` later: its `source` becomes `requested` if they accept.
///
// Everything above ships to models as the `$defs.HeldSwap` description in `open_instrument`'s
// advertised outputSchema. Notes for humans go below this line.
//
// One type, three doors, as for `Conflict`: `Response::Held`, `SwapOutcome::Held`, and
// `open_instrument`'s `held` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct HeldSwap {
    /// The source this open asked to install, which the editor's user has been asked to open.
    pub requested: String,
    /// The source still playing, when the engine knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playing: Option<String>,
    /// The content hash still playing.
    pub content_hash: String,
}

/// The document the engine is currently playing, paired with its content hash — the token to pass
/// as a later swap's `expect` guard — and the `source` it was installed from.
///
/// The document is raw JSON, exactly as installed: the engine is the single validation authority,
/// so nothing re-validates it on the way out.
///
/// **This is a channel type, not an agent-facing one.** Every arm that hands instrument
/// JSON to a model has been retired, so the `document` here reaches a *door*, never a context: the MCP door projects
/// it and returns the projection. The channel keeps carrying the whole document because its other
/// consumers (a host embedding the engine, a test) are programs, and because `source` alone cannot
/// answer "what is playing?" — the file may have moved on since the swap, which is precisely what
/// pairing `source` with the installed `content_hash` lets the agent detect.
// Only the FIELD docs below reach a model: schemars emits no root `description`, so
// `get_current_instrument`'s outputSchema root description is null and this block ships nowhere.
// (`Conflict` is the opposite case — it is referenced from `$defs`, so its block does ship.)
//
// The snapshot itself is advertised nowhere: `get_current_instrument` answers with a projection cut
// from it, so this type reaches a door and stops there. Hence no schema derive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSnapshot {
    /// The document the engine is currently playing, as raw JSON. `null` while the open
    /// instrument does not load and nothing plays.
    pub document: serde_json::Value,
    /// Its content hash — the token a later swap's `expect` guard compares. Empty while the open
    /// instrument does not load and nothing plays; `source` then names the open instrument.
    pub content_hash: String,
    /// Where the playing document was installed from, when the engine knows: the `source` of the
    /// last successful swap, or the document `reuben play` started on. `None` after an install by
    /// value (which has no source) and for the built-in default rig.
    ///
    /// The drift signal lives in the *pair*: an agent that edits this source and does not swap sees
    /// its own hash diverge from the installed one. Optional so a snapshot minted before this field
    /// existed still deserializes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// What happened to the sounding graph across a swap, keyed by the survivor fingerprint.
/// `state_reset` lists addresses present in both documents whose node did **not** survive (a type
/// change, or an instantiate-time fingerprint change) — those nodes were rebuilt cold and lost
/// their state. `added`/`removed` are relative to what was playing, so a param tweak that reports
/// `removed: ["/voice1"]` means the address was mistyped. A door that rebuilds every node reports
/// `survived: 0`.
///
// Everything above ships to models as the `$defs.DiffSummary` description in `open_instrument`'s
// advertised outputSchema, so it stays about what the fields mean. Notes for humans go below this
// line, where schemars will not pick them up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DiffSummary {
    pub survived: usize,
    pub state_reset: Vec<String>,
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// What an `open` returns: the validation report, the **installed** document's content hash (on
/// `ok: false` nothing installed — the hash still names what keeps playing), and, on success, the
/// diff summary. The report flattens so the wire shape is one flat object.
///
/// An empty hash means nothing plays: the open instrument does not load. An `open` of a document
/// that does not load answers that way, with the load's errors, and the document is then open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SwapReport {
    #[serde(flatten)]
    pub report: Report,
    pub content_hash: String,
    /// Present on a successful install; a rejected one (`ok: false`) has no old-vs-new to
    /// summarize.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffSummary>,
}

impl SwapReport {
    /// The nothing-was-installed report, defined once. A door needs this shape even when it
    /// rejects an open before the loader runs.
    ///
    /// `content_hash` is **what keeps playing** — the conflict's `actual`, never the `expected` the
    /// client asked for.
    pub fn rejected(content_hash: String) -> Self {
        Self {
            report: Report {
                ok: false,
                errors: vec![],
                warnings: vec![],
            },
            content_hash,
            diff: None,
        }
    }

    /// The window's reading of what the engine's Coordinator reported for a swap.
    pub fn from_core(report: reuben_document::SwapReport) -> Self {
        Self {
            report: Report::from_core(report.report),
            content_hash: report.content_hash,
            diff: report.diff.map(|d| DiffSummary {
                survived: d.survived,
                state_reset: d.state_reset,
                added: d.added,
                removed: d.removed,
            }),
        }
    }
}

/// One structure-channel response (one per request, in order), serialized as
/// a single JSON object tagged by `reply`. The tag rides *inside* the payload object
/// (`{"reply": "swap_report", "ok": …}`), so an open's wire shape stays the flat
/// `SwapReport` object the MCP tool also serializes (the shapes must not
/// drift) — the envelope adds a discriminant, never a nesting level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum Response {
    /// `Ping`'s answer: the channel itself is alive.
    Pong,
    /// `Swap`'s answer, success or rejection alike: the contract's [`SwapReport`]
    /// — `ok`, diagnostics, the **installed** document's content hash,
    /// and on success the diff summary.
    SwapReport(SwapReport),
    /// `GetDocument`'s answer: the [`DocumentSnapshot`] — the canonical installed document with
    /// its content hash. A newtype variant, so the payload's fields ride *flat* next to the
    /// `reply` tag exactly as they did when this variant spelled them out inline.
    Document(DocumentSnapshot),
    /// `GetDiagnostics`' answer: the diagnostics counters.
    Diagnostics(DiagnosticsReport),
    /// `GetMeters`' answer: the levels at the device, one entry per channel, and those of the
    /// nodes [`SetProbes`](Request::SetProbes) chose.
    Meters(MetersReport),
    /// `SetProbes`' answer: the paths that now measure, each as given, in request order. Fewer
    /// than were asked means the rest named no node with a Signal output, or repeated a path
    /// already given.
    Probes { resolved: Vec<Vec<String>> },
    /// A `Swap` whose `expect` guard missed: nothing installed — the [`Conflict`] names both
    /// hashes so the client re-reads via `GetDocument` and reconciles. A newtype variant, so both
    /// hashes ride the wire flat next to the `reply` tag under the user-facing field names (the
    /// tool schema is `conflict: {expected, actual}` — the same struct, not a re-wrap).
    Conflict(Conflict),
    /// A `Swap` the host held rather than installed, so its user can be asked first: nothing
    /// installed, and the [`HeldSwap`] names what keeps playing. Flat next to the `reply` tag.
    Held(HeldSwap),
    /// `Send`'s ack: the engine **received** the whole batch and queued it for the next rendered
    /// block.
    ///
    /// Not "applied": an address routing to no node/port is dropped silently at the engine's
    /// ingress, exactly as an external OSC datagram naming a stale address is.
    ///
    /// A **unit** ack, deliberately. It once carried a `count`, which could only ever equal the
    /// request's own `messages.len()`: the batch is queued as one unit, so there is no partial
    /// outcome for a count to describe, and every rejecting path answers [`Error`](Self::Error)
    /// instead. A field that can hold only one value is not information — it is an invitation to
    /// read `count < len` as "some were rejected", a signal that can never arrive. (Contrast
    /// [`Conflict`], whose two hashes earn their place precisely because they can disagree.)
    Sent,
    /// A channel-level failure: an unreadable request, or an engine-side fault that
    /// produced no domain-shaped answer. Distinct from a `SwapReport` with `ok: false`,
    /// which is the channel *working*.
    Error { message: String },
    /// `Call`'s answer when the verb ran: its output and one-line summary. An edit the engine
    /// refused is still this, with `written: false` in the output.
    ///
    /// An edit's output `hash` is the content hash of the file the edit named, not of what plays:
    /// for an edit to a document the open instrument nests, it differs from the
    /// [`Event::Changed`] hash the same edit publishes.
    Called {
        output: serde_json::Value,
        summary: String,
    },
    /// `List`'s answer: the names offered, sorted.
    Listed { names: Vec<String> },
    /// `Library`'s answer.
    Library(Library),
    /// `Surface`'s answer: the open instrument's surface text, or `None` when nothing is open or
    /// it has no surface.
    Surface { text: Option<String> },
    /// `Call`'s answer when the verb refused to run: an unknown name, an engine verb, or arguments
    /// that do not fit it.
    Refused { message: String },
    /// `Subscribe`'s answer: every line after this one on the connection is an [`Event`].
    Subscribed,
}

impl Response {
    /// Serialize as one newline-terminated JSON line — the NDJSON framing both ends write.
    pub fn to_ndjson(&self) -> String {
        to_ndjson_line(self)
    }

    /// Parse one NDJSON line (trailing newline tolerated, as `serde_json` skips trailing
    /// whitespace). Errors are the caller's to wrap — the envelope does not police I/O.
    pub fn from_ndjson(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// One change to what the engine plays, pushed to each subscriber as a single JSON object tagged
/// by `event`, one per line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    /// The open instrument changed and the new Plan is playing. `hash` is the installed top
    /// document's content hash, whichever of its files changed: after an edit to a nested
    /// document it is not the hash that edit's own result carries. An empty `hash` is a write
    /// while nothing plays: the file changed and no Plan was installed.
    Changed { hash: String, by: ChangedBy },
    /// Another instrument was opened and is playing. `name` is the host's
    /// [origin](crate::engine::EngineHost::document_origin) of the name it was opened by, the one
    /// spelling every client hears; empty for one installed by value.
    Opened { name: String, hash: String },
    /// The open instrument's documents no longer load; what played before keeps playing.
    Invalid { errors: Vec<Diag> },
}

/// Who made a [`Event::Changed`] change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangedBy {
    /// A client of the engine, through the channel.
    Client,
    /// Something outside the engine wrote a document of the open instrument.
    Outside,
}

impl Event {
    /// Serialize as one newline-terminated JSON line — the NDJSON framing both ends write.
    pub fn to_ndjson(&self) -> String {
        to_ndjson_line(self)
    }

    /// Parse one NDJSON line (trailing newline tolerated, as `serde_json` skips trailing
    /// whitespace).
    pub fn from_ndjson(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

/// The diagnostics payload: the counters, running totals since engine start, exposed
/// past log-only through this channel (its vehicle). `output_xruns` counts events; the
/// ring counters count **frames**. New counters
/// land as new fields here — this stays the one wire surface mirroring reuben-native's
/// `diagnostics.rs`, never a second parallel shape.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
pub struct DiagnosticsReport {
    /// Output render callbacks that missed their real-time budget (events).
    pub output_xruns: u64,
    /// Input frames read as zeros because the ring ran empty (frames).
    pub input_ring_underruns: u64,
    /// Oldest queued input frames discarded by the consumer-side high-water trim (frames).
    pub input_ring_overruns: u64,
    /// Incoming input frames dropped by the producer against a full ring — the
    /// stalled-consumer backstop, kept separate from overruns because the two diagnoses
    /// have opposite fixes (frames).
    pub input_ring_producer_drops: u64,
}

/// The engine's levels at the device: what it sends out, after the output map, and what the input
/// device delivers, before the input map. One entry per device channel, in channel order; a host
/// with no device, or no input stream open, answers empty arrays.
///
/// The ballistics are the engine's, applied on the audio thread as each block is metered, so every
/// client sees the same values whatever rate it polls at, and a transient between two polls still
/// shows. A client may add its own peak hold on top.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MetersReport {
    /// One per device output channel.
    pub outputs: Vec<ChannelLevel>,
    /// One per input device channel, whether or not the input map routes it anywhere.
    pub inputs: Vec<ChannelLevel>,
    /// Output frames metered since the engine started. Unchanged between two polls means the
    /// output callback is not running, and the levels are the last ones it measured. It says
    /// nothing of the input stream, whose levels hold their last value if it stops.
    pub frames: u64,
    /// The levels of the nodes [`Request::SetProbes`] chose, in the order it resolved them. Empty
    /// while none is chosen, and then absent from the JSON.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[schemars(skip)]
    pub probes: Vec<ProbeLevels>,
}

/// One probed node's levels: a [`PortLevel`] per Signal output port, in port order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeLevels {
    /// The node's path, as given to [`Request::SetProbes`].
    pub path: Vec<String>,
    /// One per Signal output port of the node.
    pub ports: Vec<PortLevel>,
}

/// One output port's level, with the same ballistics as a device channel's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortLevel {
    /// The port's name on the node.
    pub port: String,
    /// The port's peak and RMS, in dBFS.
    pub level: ChannelLevel,
}

/// One channel's level, in dBFS: 0 is a full-scale sample. Always between -120 and 60, so silence
/// reads -120 and a channel carrying infinite or NaN samples reads 60.
//
// The numbers are spelled out because this comment is advertised to a model as schema prose;
// `the_advertised_levels_name_the_constants` holds them to the constants.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ChannelLevel {
    /// The highest sample magnitude, falling back at 12 dB per second once the signal drops.
    pub peak_dbfs: f32,
    /// The RMS level, an exponential average with a 0.3 s time constant. A full-scale sine reads
    /// -3 dBFS.
    pub rms_dbfs: f32,
}

impl ChannelLevel {
    /// The level of a channel whose held peak magnitude is `peak` and whose averaged square is
    /// `mean_square`, both linear, so every host converts them the same way.
    pub fn from_linear(peak: f32, mean_square: f32) -> Self {
        Self {
            peak_dbfs: dbfs(20.0 * peak.log10()),
            rms_dbfs: dbfs(10.0 * mean_square.log10()),
        }
    }
}

/// Clamp a level into the reported range: `log10(0)` is -inf, and a NaN is a channel gone bad,
/// which says more as the ceiling than as silence.
fn dbfs(level: f32) -> f32 {
    if level.is_nan() {
        return METER_CEILING_DBFS;
    }
    level.clamp(METER_FLOOR_DBFS, METER_CEILING_DBFS)
}

/// The lowest level a [`ChannelLevel`] reports; silence reads as this.
pub const METER_FLOOR_DBFS: f32 = -120.0;
/// The highest level a [`ChannelLevel`] reports.
pub const METER_CEILING_DBFS: f32 = 60.0;
/// How fast a [`ChannelLevel::peak_dbfs`] falls back, in dB per second, and the time constant of
/// [`ChannelLevel::rms_dbfs`]'s average, in seconds. Defined by the meter in `reuben-core`.
pub use reuben_core::meter::{PEAK_FALL_DB_PER_SEC, RMS_WINDOW_SECS};

/// The longest line either end of the structure channel reads, newline included. A line is one
/// request or one response, and the largest is a `call`'s answer describing a whole document: tens
/// of kilobytes for the largest instruments in the library, so this leaves two
/// orders of magnitude of headroom while bounding what a peer that never sends a newline can make
/// the reader buffer.
pub const MAX_LINE_BYTES: usize = 4 << 20;

/// Read one NDJSON line into `line`, appending to whatever a read interrupted by a timeout left
/// there, as [`BufRead::read_line`] does, but never letting `line` grow past [`MAX_LINE_BYTES`].
///
/// A line that reaches the cap without its newline is [`io::ErrorKind::InvalidData`]. The rest of
/// it is still unread, so the stream is no longer at a line boundary and the caller should close
/// it.
pub fn read_ndjson_line(reader: &mut impl BufRead, line: &mut String) -> io::Result<usize> {
    read_line_within(reader, line, MAX_LINE_BYTES)
}

fn read_line_within(reader: &mut impl BufRead, line: &mut String, max: usize) -> io::Result<usize> {
    let room = max.saturating_sub(line.len());
    let mut limited = reader.take(room as u64);
    let read = limited.read_line(line);
    // Checked before `read`'s own error: a cap that falls inside a multibyte character fails
    // `read_line`'s UTF-8 check, and the line is still over-long, not merely malformed.
    if limited.limit() == 0 && !line.ends_with('\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a structure channel line is longer than {max} bytes"),
        ));
    }
    read
}

/// The one statement of the framing: compact JSON (serde_json never emits raw newlines —
/// string contents escape to `\n`) plus the line terminator.
fn to_ndjson_line<T: Serialize>(value: &T) -> String {
    let mut line = serde_json::to_string(value).expect("wire envelope serializes to JSON");
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;
    use crate::authoring::{Diag, Report};

    fn held_swap() -> HeldSwap {
        HeldSwap {
            requested: "instruments/groovebox.json".to_string(),
            playing: Some("instruments/acid-techno.json".to_string()),
            content_hash: "00c0ffee00c0ffee".to_string(),
        }
    }

    /// A populated SwapReport exercising every field, so the round-trip proves the envelope
    /// carries the contract type intact (shapes must not drift).
    fn swap_report() -> SwapReport {
        SwapReport {
            report: Report {
                ok: true,
                errors: vec![],
                warnings: vec![Diag {
                    node: Some("/voicer".to_string()),
                    port: None,
                    message: "missing resource".to_string(),
                }],
            },
            content_hash: "00c0ffee00c0ffee".to_string(),
            diff: Some(DiffSummary {
                survived: 0,
                state_reset: vec!["/osc".to_string()],
                added: vec![],
                removed: vec![],
            }),
        }
    }

    fn diagnostics_report() -> DiagnosticsReport {
        DiagnosticsReport {
            output_xruns: 2,
            input_ring_underruns: 480,
            input_ring_overruns: 0,
            input_ring_producer_drops: 96,
        }
    }

    #[test]
    fn a_line_within_the_cap_reads_whole_and_one_past_it_is_refused() {
        let mut line = String::new();
        let mut reader = io::Cursor::new("1234567\nnext\n");
        assert_eq!(
            read_line_within(&mut reader, &mut line, 8).expect("fits"),
            8
        );
        assert_eq!(line, "1234567\n");

        let mut line = String::new();
        let mut reader = io::Cursor::new("12345678\n");
        let err = read_line_within(&mut reader, &mut line, 8).expect_err("one byte over");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("longer than 8 bytes"), "{err}");
    }

    #[test]
    fn bytes_left_by_an_interrupted_read_count_toward_the_cap() {
        let mut line = "12345".to_string();
        let mut reader = io::Cursor::new("67\n");
        assert_eq!(
            read_line_within(&mut reader, &mut line, 8).expect("fits"),
            3
        );
        assert_eq!(line, "1234567\n");

        let mut line = "123456".to_string();
        let mut reader = io::Cursor::new("78\n");
        let err = read_line_within(&mut reader, &mut line, 8).expect_err("over with the rest");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_stream_that_ends_mid_line_is_a_short_read_not_a_refusal() {
        let mut line = String::new();
        let mut reader = io::Cursor::new("123");
        assert_eq!(read_line_within(&mut reader, &mut line, 8).expect("eof"), 3);
        assert_eq!(line, "123");
        assert_eq!(read_line_within(&mut reader, &mut line, 8).expect("eof"), 0);
    }

    #[test]
    fn a_cap_inside_a_multibyte_character_is_refused_as_over_long() {
        // 'é' is two bytes; the cap at 8 falls between them.
        let mut line = String::new();
        let mut reader = io::Cursor::new("1234567é\n");
        let err = read_line_within(&mut reader, &mut line, 8).expect_err("over the cap");
        assert!(err.to_string().contains("longer than 8 bytes"), "{err}");
    }

    fn meters_report() -> MetersReport {
        MetersReport {
            outputs: vec![
                ChannelLevel {
                    peak_dbfs: -0.5,
                    rms_dbfs: -3.5,
                },
                ChannelLevel {
                    peak_dbfs: METER_FLOOR_DBFS,
                    rms_dbfs: METER_FLOOR_DBFS,
                },
            ],
            inputs: vec![ChannelLevel {
                peak_dbfs: -18.0,
                rms_dbfs: -24.0,
            }],
            frames: 96_000,
            probes: Vec::new(),
        }
    }

    /// Every request must serialize to exactly one newline-terminated JSON line (the NDJSON
    /// framing contract) and parse back to itself.
    fn assert_one_line_round_trip(req: &Request) {
        let line = req.to_ndjson();
        assert!(
            line.ends_with('\n'),
            "an NDJSON line is newline-terminated: {line:?}"
        );
        assert_eq!(
            line.matches('\n').count(),
            1,
            "exactly one newline — one JSON object per line: {line:?}"
        );
        let back = Request::from_ndjson(&line).expect("parses back");
        assert_eq!(&back, req);
    }

    #[test]
    fn default_structure_addr_is_loopback_9124() {
        // The shared bind/dial address: loopback-only and the fixed M1 port. This
        // one const is the single source both the reuben-native server and the reuben-mcp client
        // reference, so they can never drift; the literal is pinned here so an accidental edit is
        // a red test, not a silent server/client mismatch.
        assert_eq!(DEFAULT_STRUCTURE_ADDR, "127.0.0.1:9124");
        assert!(
            DEFAULT_STRUCTURE_ADDR.starts_with("127.0.0.1:"),
            "the structure channel must stay loopback-only: {DEFAULT_STRUCTURE_ADDR}"
        );
    }

    /// A batch exercising all three atom kinds, so a round trip proves each survives.
    fn control_messages() -> Vec<ControlMessage> {
        vec![
            ControlMessage {
                address: "/filt/cutoff".to_string(),
                args: vec![ControlArg::F32(800.0)],
            },
            ControlMessage {
                address: "/lfo/shape".to_string(),
                args: vec![ControlArg::Str("Up".to_string()), ControlArg::I32(3)],
            },
        ]
    }

    #[test]
    fn every_request_variant_round_trips_as_one_ndjson_line() {
        // The verbs: ping, open (with the optional expect guard), list, get_document,
        // get_diagnostics, get_meters, set_probes, send, call.
        let requests = [
            Request::Ping,
            Request::Open {
                name: "pad.json".to_string(),
                expect: Some("00c0ffee00c0ffee".to_string()),
            },
            Request::List {
                what: ListWhat::Instruments,
            },
            Request::List {
                what: ListWhat::Samples,
            },
            Request::GetDocument,
            Request::GetDiagnostics,
            Request::GetMeters,
            Request::SetProbes {
                paths: vec![vec!["osc".to_string()]],
            },
            Request::Send {
                messages: control_messages(),
            },
            Request::Call {
                tool: "describe_instrument".to_string(),
                args: serde_json::json!({ "source": "a.json" }),
            },
            Request::Subscribe,
        ];
        for req in &requests {
            assert_one_line_round_trip(req);
        }
    }

    #[test]
    fn send_carries_its_batch_as_bare_json_atoms() {
        // The `send` verb's wire shape is the contract: one verb carrying the whole batch
        // (so a gesture cannot half-apply), each message an `{address, args}` pair, and each atom
        // its BARE JSON value — untagged, so the line stays readable over netcat.
        let v: serde_json::Value = serde_json::from_str(
            &Request::Send {
                messages: control_messages(),
            }
            .to_ndjson(),
        )
        .expect("as value");
        assert_eq!(
            v,
            serde_json::json!({
                "verb": "send",
                "messages": [
                    { "address": "/filt/cutoff", "args": [800.0] },
                    { "address": "/lfo/shape", "args": ["Up", 3] }
                ]
            })
        );
    }

    #[test]
    fn control_args_split_integers_from_floats() {
        // The variant order in `ControlArg` is load-bearing: untagged deserialization takes the
        // first variant that fits, so `I32` before `F32` is what keeps a JSON integer an integer.
        // Flip the order and every one of these integer cases silently becomes an F32.
        let parse = |json: &str| serde_json::from_str::<ControlArg>(json).expect("atom parses");
        assert_eq!(parse("3"), ControlArg::I32(3));
        assert_eq!(parse("-7"), ControlArg::I32(-7));
        assert_eq!(parse("800.0"), ControlArg::F32(800.0));
        assert_eq!(parse("0.5"), ControlArg::F32(0.5));
        assert_eq!(parse("\"Up\""), ControlArg::Str("Up".to_string()));

        // The MIDI-note case, the one most likely to be silently mangled: an f32 with an integral
        // value must NOT come back as an I32. serde_json emits `69.0` with its `.0` intact and
        // `deserialize_i32` rejects a float-backed number, so the round trip holds.
        let line = serde_json::to_string(&ControlArg::F32(69.0)).expect("serializes");
        assert_eq!(line, "69.0");
        assert_eq!(parse(&line), ControlArg::F32(69.0));

        // A number too large for i32 falls through to F32 rather than failing the batch.
        assert_eq!(
            parse("3000000000"),
            ControlArg::F32(3_000_000_000_f64 as f32)
        );
    }

    #[test]
    fn control_args_become_the_engines_primitive_args() {
        // The conversion into the engine's own enum is the whole point of the type: three
        // primitives in, the three primitive `Arg`s out, and nothing else can be spelled.
        assert_eq!(Arg::from(ControlArg::I32(3)), Arg::I32(3));
        assert_eq!(Arg::from(ControlArg::F32(0.5)), Arg::F32(0.5));
        assert_eq!(
            Arg::from(ControlArg::Str("Up".to_string())),
            Arg::Str("Up".into())
        );
    }

    #[test]
    fn requests_speak_the_adr_0046_verbs() {
        // The wire shape is the contract, not an implementation detail: every request is an
        // object tagged by `verb`, named exactly as the channel names the four verbs.
        let v: serde_json::Value =
            serde_json::from_str(&Request::Ping.to_ndjson()).expect("ping as value");
        assert_eq!(v, serde_json::json!({ "verb": "ping" }));

        let v: serde_json::Value =
            serde_json::from_str(&Request::GetDocument.to_ndjson()).expect("as value");
        assert_eq!(v, serde_json::json!({ "verb": "get_document" }));

        let v: serde_json::Value =
            serde_json::from_str(&Request::GetDiagnostics.to_ndjson()).expect("as value");
        assert_eq!(v, serde_json::json!({ "verb": "get_diagnostics" }));

        let v: serde_json::Value = serde_json::from_str(
            &Request::Send {
                messages: control_messages(),
            }
            .to_ndjson(),
        )
        .expect("as value");
        assert_eq!(v["verb"], serde_json::json!("send"));
    }

    #[test]
    fn responses_speak_their_reply_tags() {
        // The wire shape is the contract, not an implementation detail: every response is an
        // object tagged by `reply`, named exactly as the channel names each answer. Pinned as
        // literals here (the way requests pin their `verb` names) so a rename is a red unit test,
        // not a silent break of the reuben-mcp client that keys off these tags.
        let tag = |resp: &Response| -> String {
            let v: serde_json::Value =
                serde_json::from_str(&resp.to_ndjson()).expect("response as value");
            v["reply"]
                .as_str()
                .expect("reply tag is a string")
                .to_string()
        };
        assert_eq!(tag(&Response::Pong), "pong");
        assert_eq!(tag(&Response::SwapReport(swap_report())), "swap_report");
        assert_eq!(
            tag(&Response::Document(DocumentSnapshot {
                document: serde_json::json!({}),
                content_hash: String::new(),
                source: None,
            })),
            "document"
        );
        assert_eq!(
            tag(&Response::Diagnostics(diagnostics_report())),
            "diagnostics"
        );
        assert_eq!(tag(&Response::Meters(meters_report())), "meters");
        assert_eq!(tag(&Response::Sent), "sent");
        assert_eq!(
            tag(&Response::Conflict(Conflict {
                expected: String::new(),
                actual: String::new(),
            })),
            "conflict"
        );
        assert_eq!(tag(&Response::Held(held_swap())), "held");
        assert_eq!(
            tag(&Response::Called {
                output: serde_json::json!({}),
                summary: String::new(),
            }),
            "called"
        );
        assert_eq!(
            tag(&Response::Refused {
                message: String::new(),
            }),
            "refused"
        );
        assert_eq!(
            tag(&Response::Error {
                message: String::new(),
            }),
            "error"
        );
        assert_eq!(tag(&Response::Subscribed), "subscribed");
    }

    /// Same framing contract as [`assert_one_line_round_trip`], response side.
    fn assert_one_line_response_round_trip(resp: &Response) {
        let line = resp.to_ndjson();
        assert!(
            line.ends_with('\n'),
            "an NDJSON line is newline-terminated: {line:?}"
        );
        assert_eq!(
            line.matches('\n').count(),
            1,
            "exactly one newline — one JSON object per line: {line:?}"
        );
        let back = Response::from_ndjson(&line).expect("parses back");
        assert_eq!(&back, resp);
    }

    #[test]
    fn every_response_variant_round_trips_as_one_ndjson_line() {
        let responses = [
            Response::Pong,
            Response::SwapReport(swap_report()),
            Response::Document(DocumentSnapshot {
                document: serde_json::json!({
                    "format_version": 3,
                    "instrument": "t",
                    "nodes": []
                }),
                content_hash: "00c0ffee00c0ffee".to_string(),
                source: Some("voices/lead.json".to_string()),
            }),
            Response::Diagnostics(diagnostics_report()),
            Response::Meters(meters_report()),
            Response::Meters(MetersReport::default()),
            Response::Sent,
            Response::Conflict(Conflict {
                expected: "0badc0de0badc0de".to_string(),
                actual: "00c0ffee00c0ffee".to_string(),
            }),
            Response::Held(held_swap()),
            Response::Listed {
                names: vec!["a.json".to_string()],
            },
            Response::Library(Library {
                instruments: vec![LibraryEntry {
                    name: "a.json".to_string(),
                    title: Some("A".to_string()),
                    description: None,
                    error: None,
                    reads: vec!["a.json".to_string()],
                }],
                dirs: vec!["".to_string()],
            }),
            Response::Surface { text: None },
            Response::Error {
                message: "unreadable request".to_string(),
            },
            Response::Called {
                output: serde_json::json!({ "written": true }),
                summary: "a.json: renamed".to_string(),
            },
            Response::Refused {
                message: "unknown tool `x`".to_string(),
            },
            Response::Subscribed,
        ];
        for resp in &responses {
            assert_one_line_response_round_trip(resp);
        }
    }

    #[test]
    fn events_speak_their_tags_and_round_trip_as_one_ndjson_line() {
        let events = [
            (
                Event::Changed {
                    hash: "00c0ffee00c0ffee".to_string(),
                    by: ChangedBy::Client,
                },
                serde_json::json!({"event": "changed", "hash": "00c0ffee00c0ffee", "by": "client"}),
            ),
            (
                Event::Changed {
                    hash: "00c0ffee00c0ffee".to_string(),
                    by: ChangedBy::Outside,
                },
                serde_json::json!({"event": "changed", "hash": "00c0ffee00c0ffee", "by": "outside"}),
            ),
            (
                Event::Opened {
                    name: "pad.json".to_string(),
                    hash: "00c0ffee00c0ffee".to_string(),
                },
                serde_json::json!({"event": "opened", "name": "pad.json", "hash": "00c0ffee00c0ffee"}),
            ),
            (
                Event::Invalid {
                    errors: vec![Diag {
                        node: None,
                        port: None,
                        message: "unknown operator".to_string(),
                    }],
                },
                serde_json::json!({"event": "invalid", "errors": [{"message": "unknown operator"}]}),
            ),
        ];
        for (event, wire) in &events {
            let line = event.to_ndjson();
            assert_eq!(line.matches('\n').count(), 1, "{line:?}");
            assert!(line.ends_with('\n'), "{line:?}");
            let value: serde_json::Value = serde_json::from_str(&line).expect("event as value");
            assert_eq!(&value, wire);
            assert_eq!(&Event::from_ndjson(&line).expect("parses back"), event);
        }
    }

    #[test]
    fn swap_report_response_stays_the_flat_contract_object() {
        // The structure channel's swap response and the MCP tool's
        // structuredContent serialize the *same* contract type — the envelope adds only its
        // `reply` tag next to the flat { ok, errors, warnings, content_hash, diff } object,
        // never a nesting level the tool shape doesn't have.
        let v: serde_json::Value =
            serde_json::from_str(&Response::SwapReport(swap_report()).to_ndjson())
                .expect("as value");
        assert_eq!(v["reply"], serde_json::json!("swap_report"));
        assert_eq!(v["ok"], serde_json::json!(true));
        assert_eq!(v["content_hash"], serde_json::json!("00c0ffee00c0ffee"));
        assert_eq!(v["warnings"][0]["node"], serde_json::json!("/voicer"));
        assert_eq!(v["diff"]["state_reset"], serde_json::json!(["/osc"]));
    }

    #[test]
    fn diagnostics_response_carries_the_four_adr_0038_counters() {
        // The diagnostics endpoint is this channel verb, and
        // its payload is the four running totals (xruns count events, ring counters count
        // frames), flat next to the tag so new counters land as new fields.
        let v: serde_json::Value =
            serde_json::from_str(&Response::Diagnostics(diagnostics_report()).to_ndjson())
                .expect("as value");
        assert_eq!(
            v,
            serde_json::json!({
                "reply": "diagnostics",
                "output_xruns": 2,
                "input_ring_underruns": 480,
                "input_ring_overruns": 0,
                "input_ring_producer_drops": 96
            })
        );
    }

    #[test]
    fn meters_response_carries_one_level_per_channel_flat_beside_the_tag() {
        let v: serde_json::Value =
            serde_json::from_str(&Response::Meters(meters_report()).to_ndjson()).expect("as value");
        assert_eq!(
            v,
            serde_json::json!({
                "reply": "meters",
                "outputs": [
                    { "peak_dbfs": -0.5, "rms_dbfs": -3.5 },
                    { "peak_dbfs": -120.0, "rms_dbfs": -120.0 }
                ],
                "inputs": [{ "peak_dbfs": -18.0, "rms_dbfs": -24.0 }],
                "frames": 96000
            })
        );
    }

    #[test]
    fn set_probes_round_trips() {
        assert_one_line_round_trip(&Request::SetProbes {
            paths: vec![vec!["voice".to_string(), "osc".to_string()], vec![]],
        });
        assert_one_line_response_round_trip(&Response::Probes {
            resolved: vec![vec!["voice".to_string(), "osc".to_string()]],
        });
    }

    #[test]
    fn a_report_without_probes_serializes_as_before() {
        let v = serde_json::to_value(meters_report()).expect("as value");
        assert_eq!(
            v,
            serde_json::json!({
                "outputs": [
                    { "peak_dbfs": -0.5, "rms_dbfs": -3.5 },
                    { "peak_dbfs": -120.0, "rms_dbfs": -120.0 }
                ],
                "inputs": [{ "peak_dbfs": -18.0, "rms_dbfs": -24.0 }],
                "frames": 96000
            })
        );
        let mut with = meters_report();
        with.probes = vec![ProbeLevels {
            path: vec!["osc".to_string()],
            ports: vec![PortLevel {
                port: "out".to_string(),
                level: ChannelLevel {
                    peak_dbfs: -6.0,
                    rms_dbfs: -9.0,
                },
            }],
        }];
        assert_one_line_response_round_trip(&Response::Meters(with));
    }

    #[test]
    fn a_level_is_clamped_so_silence_and_a_blown_up_channel_still_serialize() {
        let silent = ChannelLevel::from_linear(0.0, 0.0);
        assert_eq!(silent.peak_dbfs, METER_FLOOR_DBFS);
        assert_eq!(silent.rms_dbfs, METER_FLOOR_DBFS);

        let blown = ChannelLevel::from_linear(f32::INFINITY, f32::NAN);
        assert_eq!(blown.peak_dbfs, METER_CEILING_DBFS);
        assert_eq!(blown.rms_dbfs, METER_CEILING_DBFS);

        let full = ChannelLevel::from_linear(1.0, 0.5);
        assert_eq!(full.peak_dbfs, 0.0);
        assert!((full.rms_dbfs + 3.0103).abs() < 1e-3, "{full:?}");

        let report = MetersReport {
            outputs: vec![silent, blown],
            ..MetersReport::default()
        };
        assert_one_line_response_round_trip(&Response::Meters(report));
    }

    /// The level prose is advertised to a model, so it spells the numbers out rather than linking
    /// the constants; this keeps the two from drifting apart.
    #[test]
    fn the_advertised_levels_name_the_constants() {
        let schema = serde_json::to_value(schemars::schema_for!(ChannelLevel)).expect("schema");
        let described = |at: &str| {
            schema
                .pointer(at)
                .and_then(|d| d.as_str())
                .expect("described")
                .replace('\n', " ")
        };
        let level = described("/description");
        assert!(
            level.contains(&format!(
                "between {METER_FLOOR_DBFS} and {METER_CEILING_DBFS}"
            )),
            "{level}"
        );
        let peak = described("/properties/peak_dbfs/description");
        assert!(
            peak.contains(&format!("{PEAK_FALL_DB_PER_SEC} dB per second")),
            "{peak}"
        );
        let rms = described("/properties/rms_dbfs/description");
        assert!(rms.contains(&format!("{RMS_WINDOW_SECS} s")), "{rms}");

        let tool = crate::engine::prose::GET_ENGINE_METERS;
        for spelled in [
            format!("{METER_FLOOR_DBFS} floor"),
            format!("{PEAK_FALL_DB_PER_SEC} dB a second"),
            format!("{RMS_WINDOW_SECS} s"),
        ] {
            assert!(tool.contains(&spelled), "{spelled} not in {tool}");
        }
    }

    #[test]
    fn conflict_and_error_responses_name_their_cause() {
        // An expect mismatch rejects the swap with a conflict naming the actual
        // hash, so the client re-reads and reconciles. The user-facing schema is
        // `conflict: {expected, actual}` — both hashes, same field names — so the tool
        // ticket composes its report from this variant field-for-field instead of threading
        // request state (shapes must not drift).
        let v: serde_json::Value = serde_json::from_str(
            &Response::Conflict(Conflict {
                expected: "0badc0de0badc0de".to_string(),
                actual: "00c0ffee00c0ffee".to_string(),
            })
            .to_ndjson(),
        )
        .expect("as value");
        assert_eq!(
            v,
            serde_json::json!({
                "reply": "conflict",
                "expected": "0badc0de0badc0de",
                "actual": "00c0ffee00c0ffee"
            })
        );

        let v: serde_json::Value = serde_json::from_str(
            &Response::Error {
                message: "unreadable request".to_string(),
            }
            .to_ndjson(),
        )
        .expect("as value");
        assert_eq!(
            v,
            serde_json::json!({ "reply": "error", "message": "unreadable request" })
        );
    }

    #[test]
    fn document_response_pairs_the_doc_with_its_content_hash() {
        // Every get_document response carries the installed document's content
        // hash, the token a later swap's expect guard compares.
        let v: serde_json::Value = serde_json::from_str(
            &Response::Document(DocumentSnapshot {
                document: serde_json::json!({ "instrument": "t" }),
                content_hash: "00c0ffee00c0ffee".to_string(),
                source: None,
            })
            .to_ndjson(),
        )
        .expect("as value");
        assert_eq!(
            v,
            serde_json::json!({
                "reply": "document",
                "document": { "instrument": "t" },
                "content_hash": "00c0ffee00c0ffee"
            })
        );
    }

    #[test]
    fn embedded_newlines_escape_instead_of_breaking_the_framing() {
        // The framing survives hostile content: arguments whose strings contain raw
        // newlines must still serialize to one line (serde_json escapes to \n), or a
        // netcat/read_line peer would split one message into garbage twice over.
        let req = Request::Call {
            tool: "set_instrument_name".to_string(),
            args: serde_json::json!({ "source": "a.json", "name": "line\nbreak" }),
        };
        assert_one_line_round_trip(&req);
    }

    /// `get_diagnostics` is a tool whose `outputSchema` a door derives from this payload type via
    /// schemars, exactly as the result shapes are.
    #[test]
    fn diagnostics_report_schema_has_the_four_counters() {
        let schema =
            serde_json::to_value(schemars::schema_for!(DiagnosticsReport)).expect("schema");
        let props = schema["properties"]
            .as_object()
            .expect("DiagnosticsReport schema has properties");
        let required = schema["required"].as_array().expect("required list");
        for field in [
            "output_xruns",
            "input_ring_underruns",
            "input_ring_overruns",
            "input_ring_producer_drops",
        ] {
            assert!(props.contains_key(field), "missing {field}: {schema}");
            assert!(
                required.contains(&serde_json::json!(field)),
                "{field} must be required: {schema}"
            );
        }
    }

    #[test]
    fn a_client_cannot_ask_to_swap_a_document_in() {
        for line in [
            r#"{"verb":"swap","source":{"path":"instruments/warm-pad.json"}}"#,
            r#"{"verb":"swap","source":{"document":{"instrument":"t"}},"expect":"00c0ffee"}"#,
        ] {
            assert!(Request::from_ndjson(line).is_err(), "{line}");
        }
    }
}
