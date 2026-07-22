//! The structure channel's NDJSON wire envelope: the shared `Request`/
//! `Response` types the native server (in `reuben play`) and the reuben-mcp client both
//! serialize, one JSON object per line, one response per request in order.
//!
//! No engine, no threads, no sockets — and the TCP server is reuben-native's, the client
//! reuben-mcp's.
//!
//! # What lives here versus in `contract`
//!
//! **`contract` holds what core itself produces; `wire` holds the shape choices this channel
//! makes.** `Coordinator::swap_document` returns a
//! [`SwapReport`](crate::contract::SwapReport), so that type — and `Report`, `Diag`,
//! `DiffSummary`, [`content_hash`](crate::contract::content_hash) — is door-agnostic and lives in
//! [`crate::contract`], shared by every door. This module owns the envelope (verbs, `reply` tags,
//! framing) *plus* the payloads that exist only because this channel exists:
//! [`DiagnosticsReport`], [`Conflict`], and [`DocumentSnapshot`].
//!
//! The rule is about *payload types*. [`DEFAULT_STRUCTURE_ADDR`] is a deliberate exception: it
//! lives here because both ends must agree on one literal, and next to the types both ends
//! serialize is where that agreement is hardest to break. (The engine's OSC-in port used to sit
//! beside it for the same reason, back when the sidecar dialed OSC. It no longer does — control
//! rides this channel now, and OSC-the-wire is only `reuben play`'s foreign edge — so the port
//! moved to `reuben_native::osc`, which owns that edge and is its only consumer.)
//!
//! [`Conflict`] is the worked example. Core has no conflict type — and no `expect` guard to
//! produce one: `swap_document` is last-write-wins, and the optimistic-concurrency guard is a door
//! concern (see rules: agent-mcp). *This channel* decides that its clients get a guard and that a
//! miss is a distinct answer rather than a rejected report, so the type is this channel's. Likewise
//! [`DocumentSnapshot`]: core exposes `document()` and `installed_hash()` separately, and pairing
//! them is a wire shape. Ask "would this type still mean anything with the structure channel
//! deleted?" — if no, it belongs here.
//!
//! Framing is newline-delimited JSON
//! ([`Request::to_ndjson`]/[`Request::from_ndjson`] and the `Response` pair) so the channel
//! stays netcat-debuggable and std-only.
//!
//! see rules: execution-runtime

use serde::{Deserialize, Serialize};

use crate::contract::SwapReport;
use crate::message::Arg;

/// The structure channel's default loopback bind/target: `127.0.0.1` only —
/// structure edits are more powerful than OSC control, so unlike OSC's `0.0.0.0:9000` this
/// channel must never be network-exposed. The concrete port is epic-level detail; a fixed
/// default suffices for M1. Shared here — next to the wire types both ends serialize — so the
/// reuben-native server (`reuben play`) and the reuben-mcp client bind and dial the *same*
/// address and can never drift; a taken port is non-fatal on the server side (see `play`).
pub const DEFAULT_STRUCTURE_ADDR: &str = "127.0.0.1:9124";

/// Where a swap's document comes from (accepted **by value or by path**, both
/// resolver-loaded engine-side; both branches deliberately stay open, and the
/// channel keeps both — which to *expose* is the tool surface's call). Field
/// names match that tool contract: `document` for inline JSON, `path` for a
/// resolver-anchored location.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocSource {
    /// The whole document inline, as raw JSON. Raw [`serde_json::Value`] — not a parsed doc
    /// type — because validation is the engine side's job (the loader is the single
    /// validation authority); the envelope never pre-judges a document.
    Document(serde_json::Value),
    /// A path the engine side loads through its resolver (near-zero tokens for the
    /// dev-with-checkout persona).
    Path(String),
}

/// One control atom on this channel: the **flat primitive form** — a number or a string, and
/// nothing else.
///
/// This is deliberately *not* [`Arg`], the central engine enum. `Arg` also carries `Note`,
/// `Harmony`, `Pitch`, `Enum`, and a whole `F32Buffer` — a vocabulary a control channel would
/// immediately have to forbid. Every door ships `{address, [Arg]}` in its **own local framing**
/// (reuben-web hand-rolls a flat codec, `reuben play`'s foreign edge speaks OSC-the-binary-protocol);
/// this is the structure channel's, and each converges at
/// [`Engine::queue_osc`](crate::engine::Engine::queue_osc), where the destination port's declared
/// type drives the conversion to the single typed `Arg` it carries
/// ([`osc_in_arg`](crate::boundary::osc_in_arg)). So the primitives are all this type needs to
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

/// One structure-channel request (its five verbs), serialized as a single JSON
/// object tagged by `verb`: `{"verb": "ping"}`, `{"verb": "swap", "source": …}`, ….
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum Request {
    /// Liveness — the structure channel's own probe: it proves the structure channel
    /// itself, which an OSC ping does not.
    Ping,
    /// Install a document (whole-document edit contract).
    Swap {
        /// The document, by value or by path.
        source: DocSource,
        /// The opt-in concurrency guard: the [`content_hash`](crate::contract::content_hash)
        /// the client believes is installed. A mismatch rejects the swap with
        /// `Response::Conflict` — no sessions, no leases, one off-thread hash compare.
        /// `None` is last-write-wins, the default arbitration.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect: Option<String>,
    },
    /// Read the currently installed document — the Coordinator owns the canonical doc,
    /// so a fresh conversation attaches in one call.
    GetDocument,
    /// Read the diagnostics counters, exposed past log-only (this channel is their
    /// vehicle).
    GetDiagnostics,
    /// Audition a batch of control values on the running engine — the **control plane**, riding
    /// the same channel as the structure verbs.
    ///
    /// Batched because the authoring gesture is multi-control: one exchange delivers the whole
    /// batch, so a gesture cannot half-apply and a concurrent client cannot interleave into the
    /// middle of it. The engine hands each message to the same ingress a decoded external OSC
    /// datagram reaches, so routing, typing, and block-quantized timing are identical — this
    /// channel carries no wire format of its own past [`ControlArg`].
    Send {
        /// The messages to apply, in order (a batch of at least one).
        messages: Vec<ControlMessage>,
    },
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

/// An `expect`-guard miss: the swap was rejected and **nothing was installed** — the engine is
/// still playing what it was playing before. Re-read the installed document, then retry against
/// the hash that is actually installed.
///
// Everything above ships to models — it becomes the `$defs.Conflict` description in the `swap`
// tool's advertised outputSchema — so it stays about what the model should DO. Notes for humans go
// below this line, where schemars will not pick them up.
//
// One type, three doors: `Response::Conflict`, the reuben-mcp client's `SwapOutcome::Conflict`, and
// the `swap` tool's `conflict` field all carry this struct, so the shapes cannot drift. Why it is a
// wire type rather than a contract type: see the module header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Conflict {
    /// The content hash the client asserted was installed.
    pub expected: String,
    /// The content hash actually still playing — re-read with `get_current_instrument` to
    /// reconcile, then retry the swap with this as `expect`.
    pub actual: String,
}

/// The document the engine is currently playing, paired with its content hash — the token to pass
/// as a later swap's `expect` guard.
///
/// The document is raw JSON, exactly as installed: the engine is the single validation authority,
/// so nothing re-validates it on the way out.
// Only the FIELD docs below reach a model: schemars emits no root `description`, so
// `get_current_instrument`'s outputSchema root description is null and this block ships nowhere.
// (`Conflict` is the opposite case — it is referenced from `$defs`, so its block does ship.)
//
// One type, three doors: `Response::Document`, the reuben-mcp client's return, and the
// `get_current_instrument` tool's `structuredContent` are all this struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DocumentSnapshot {
    /// The document the engine is currently playing, as raw JSON.
    pub document: serde_json::Value,
    /// Its content hash — the token a later swap's `expect` guard compares.
    pub content_hash: String,
}

/// One structure-channel response (one per request, in order), serialized as
/// a single JSON object tagged by `reply`. The tag rides *inside* the payload object
/// (`{"reply": "swap_report", "ok": …}`), so a swap's wire shape stays the flat
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
    /// A `Swap` whose `expect` guard missed: nothing installed — the [`Conflict`] names both
    /// hashes so the client re-reads via `GetDocument` and reconciles. A newtype variant, so both
    /// hashes ride the wire flat next to the `reply` tag under the user-facing field names (the
    /// tool schema is `conflict: {expected, actual}` — the same struct, not a re-wrap).
    Conflict(Conflict),
    /// `Send`'s ack: the engine **received** the batch and queued it for the next rendered block.
    ///
    /// Not "applied": an address routing to no node/port is dropped silently at the engine's
    /// ingress, exactly as an external OSC datagram naming a stale address is. The count is the
    /// engine's own report of what it queued rather than something the client infers from its
    /// request — the same reason [`Conflict`] carries both hashes.
    Sent {
        /// How many messages were queued.
        count: usize,
    },
    /// A channel-level failure: an unreadable request, or an engine-side fault that
    /// produced no domain-shaped answer. Distinct from a `SwapReport` with `ok: false`,
    /// which is the channel *working*.
    Error { message: String },
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

/// The diagnostics payload: the counters, running totals since engine start, exposed
/// past log-only through this channel (its vehicle). `output_xruns` counts events; the
/// ring counters count **frames**. New counters
/// land as new fields here — this stays the one wire surface mirroring reuben-native's
/// `diagnostics.rs`, never a second parallel shape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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

/// The one statement of the framing: compact JSON (serde_json never emits raw newlines —
/// string contents escape to `\n`) plus the line terminator.
fn to_ndjson_line<T: Serialize>(value: &T) -> String {
    let mut line = serde_json::to_string(value).expect("wire envelope serializes to JSON");
    line.push('\n');
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Diag, DiffSummary, Report};

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
        // The five verbs: ping, swap (by value or by path, with the optional
        // expect guard), get_document, get_diagnostics, send.
        let requests = [
            Request::Ping,
            Request::Swap {
                source: DocSource::Path("instruments/warm-pad.json".to_string()),
                expect: None,
            },
            Request::Swap {
                source: DocSource::Document(serde_json::json!({
                    "format_version": 3,
                    "instrument": "t",
                    "nodes": []
                })),
                expect: Some("00c0ffee00c0ffee".to_string()),
            },
            Request::GetDocument,
            Request::GetDiagnostics,
            Request::Send {
                messages: control_messages(),
            },
        ];
        for req in &requests {
            assert_one_line_round_trip(req);
        }
    }

    #[test]
    fn send_carries_its_batch_as_bare_json_atoms() {
        // The control plane's wire shape is the contract: one `send` verb carrying the whole batch
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
                messages: Vec::new(),
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
            })),
            "document"
        );
        assert_eq!(
            tag(&Response::Diagnostics(diagnostics_report())),
            "diagnostics"
        );
        assert_eq!(tag(&Response::Sent { count: 2 }), "sent");
        assert_eq!(
            tag(&Response::Conflict(Conflict {
                expected: String::new(),
                actual: String::new(),
            })),
            "conflict"
        );
        assert_eq!(
            tag(&Response::Error {
                message: String::new(),
            }),
            "error"
        );
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
            }),
            Response::Diagnostics(diagnostics_report()),
            Response::Sent { count: 2 },
            Response::Conflict(Conflict {
                expected: "0badc0de0badc0de".to_string(),
                actual: "00c0ffee00c0ffee".to_string(),
            }),
            Response::Error {
                message: "unreadable request".to_string(),
            },
        ];
        for resp in &responses {
            assert_one_line_response_round_trip(resp);
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
        // The framing survives hostile content: a document whose strings contain raw
        // newlines must still serialize to one line (serde_json escapes to \n), or a
        // netcat/read_line peer would split one message into garbage twice over.
        let req = Request::Swap {
            source: DocSource::Document(serde_json::json!({
                "instrument": "line\nbreak",
                "nodes": []
            })),
            expect: None,
        };
        assert_one_line_round_trip(&req);
    }

    /// `get_diagnostics` is a tool whose `outputSchema` rmcp derives from this
    /// payload type via schemars, exactly as the contract types do. Run with
    /// `--features schemars`.
    #[cfg(feature = "schemars")]
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
    fn swap_carries_its_source_by_path_or_by_value() {
        // Both branches stay open and the channel keeps both:
        // `source` is either `{"path": ...}` (resolver-loaded engine-side) or
        // `{"document": {...}}` (inline JSON). Field names match the tool contract.
        let by_path = Request::Swap {
            source: DocSource::Path("instruments/warm-pad.json".to_string()),
            expect: None,
        };
        let v: serde_json::Value = serde_json::from_str(&by_path.to_ndjson()).expect("as value");
        assert_eq!(
            v,
            serde_json::json!({
                "verb": "swap",
                "source": { "path": "instruments/warm-pad.json" }
            })
        );
        assert!(
            v.as_object().is_some_and(|s| !s.contains_key("expect")),
            "a swap without a guard omits expect entirely: {v}"
        );

        let by_value = Request::Swap {
            source: DocSource::Document(serde_json::json!({ "instrument": "t" })),
            expect: Some("00c0ffee00c0ffee".to_string()),
        };
        let v: serde_json::Value = serde_json::from_str(&by_value.to_ndjson()).expect("as value");
        assert_eq!(
            v,
            serde_json::json!({
                "verb": "swap",
                "source": { "document": { "instrument": "t" } },
                "expect": "00c0ffee00c0ffee"
            })
        );
    }
}
