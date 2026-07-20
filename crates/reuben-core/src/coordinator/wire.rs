//! The structure channel's NDJSON wire envelope: the shared `Request`/
//! `Response` types the native server (in `reuben play`) and the reuben-mcp client both
//! serialize, one JSON object per line, one response per request in order.
//!
//! This module owns the **envelope only** — no engine, no threads, no sockets. The value
//! types a response carries ([`SwapReport`](crate::contract::SwapReport),
//! [`content_hash`](crate::contract::content_hash)) live in [`crate::contract`]
//! (one schema, two doors); the TCP server is reuben-native's, the client
//! reuben-mcp's. Framing is newline-delimited JSON
//! ([`Request::to_ndjson`]/[`Request::from_ndjson`] and the `Response` pair) so the channel
//! stays netcat-debuggable and std-only.
//!
//! see rules: execution-runtime

use serde::{Deserialize, Serialize};

use crate::contract::SwapReport;

/// The structure channel's default loopback bind/target: `127.0.0.1` only —
/// structure edits are more powerful than OSC control, so unlike OSC's `0.0.0.0:9000` this
/// channel must never be network-exposed. The concrete port is epic-level detail; a fixed
/// default suffices for M1. Shared here — next to the wire types both ends serialize — so the
/// reuben-native server (`reuben play`) and the reuben-mcp client bind and dial the *same*
/// address and can never drift; a taken port is non-fatal on the server side (see `play`).
pub const DEFAULT_STRUCTURE_ADDR: &str = "127.0.0.1:9124";

/// The default UDP port for the engine's OSC-in control plane. The single source of
/// truth for the port `reuben play` binds and the reuben-mcp sidecar dials, so the two can never
/// drift on it. Only the *port* is shared: the engine binds it on `0.0.0.0` (all interfaces) while
/// the host-sharing sidecar dials it on `127.0.0.1`, so each side composes its own `host:port`
/// from this one const — unlike the structure channel above, whose full loopback address is fixed.
/// Kept next to [`DEFAULT_STRUCTURE_ADDR`] as the other endpoint the sidecar and engine must agree
/// on; a bare `u16` carries no dependency, so reuben-core stays OS-free.
pub const DEFAULT_OSC_PORT: u16 = 9000;

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

/// One structure-channel request (its four verbs), serialized as a single JSON
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
    /// `GetDocument`'s answer: the canonical installed document (raw JSON — the canonical
    /// doc is the Coordinator's, and re-validating it client-side would make two
    /// authorities) with its content hash, the token a later swap's `expect` compares.
    Document {
        document: serde_json::Value,
        content_hash: String,
    },
    /// `GetDiagnostics`' answer: the diagnostics counters.
    Diagnostics(DiagnosticsReport),
    /// A `Swap` whose `expect` guard missed: nothing installed; `actual` is
    /// the hash of what actually kept playing — the client re-reads via `GetDocument` and
    /// reconciles. Both hashes ride the wire with the user-facing field names (the
    /// schema is `conflict: {expected, actual}`), so the tool surface re-serializes this
    /// variant field-for-field instead of threading request state.
    Conflict { expected: String, actual: String },
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

    #[test]
    fn every_request_variant_round_trips_as_one_ndjson_line() {
        // The four verbs: ping, swap (by value or by path, with the optional
        // expect guard), get_document, get_diagnostics.
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
        ];
        for req in &requests {
            assert_one_line_round_trip(req);
        }
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
            Response::Document {
                document: serde_json::json!({
                    "format_version": 3,
                    "instrument": "t",
                    "nodes": []
                }),
                content_hash: "00c0ffee00c0ffee".to_string(),
            },
            Response::Diagnostics(diagnostics_report()),
            Response::Conflict {
                expected: "0badc0de0badc0de".to_string(),
                actual: "00c0ffee00c0ffee".to_string(),
            },
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
            &Response::Conflict {
                expected: "0badc0de0badc0de".to_string(),
                actual: "00c0ffee00c0ffee".to_string(),
            }
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
            &Response::Document {
                document: serde_json::json!({ "instrument": "t" }),
                content_hash: "00c0ffee00c0ffee".to_string(),
            }
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
