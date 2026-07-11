//! reuben-mcp — the per-conversation MCP stdio sidecar (ADR-0044).
//!
//! The MCP client spawns this shim over stdio; it hosts the pure introspection tools in-process
//! and reaches a user-owned `reuben play` for the engine tools. This crate is the FIRST workspace
//! member allowed an async runtime: rmcp + tokio live here and nowhere else, fenced out of every
//! other member so the play/CLI/web builds stay std-only (ADR-0044 §3/§5).
//!
//! # Tool surface (ADR-0048 §1)
//!
//! A [`ServerHandler`] declaring the `tools` and `resources` capabilities plus an `instructions`
//! field, and a tool router with all **eight** tools (ADR-0048 §1). The three pure tools
//! (`describe_operators`/`describe_instrument`/`validate`, #316) descend to
//! [`reuben_core::introspect`] over a [`reuben_native::resources::FsResolver`]; the five engine
//! tools (`send`/`engine_status`/`swap`/`get_current_instrument`/`get_diagnostics`, #318) reach a
//! user-owned `reuben play` through the [`EngineChannel`] seam:
//!
//! - `send` → [`reuben_native::osc::encode`] over the OSC/UDP control path (probe-first liveness,
//!   ADR-0048 §5).
//! - `engine_status`/`swap`/`get_current_instrument`/`get_diagnostics` → the structure channel's
//!   four verbs (ADR-0046 §8), via [`StructureClient`] behind [`EngineLink`].
//!
//! Error-layer discipline (ADR-0048 §3): a failing validation or a rejected swap is an ORDINARY
//! result — the tool worked; `isError` is reserved for the can't-do-the-job cases (an unreachable
//! engine, a bad one-of, an unknown operator). `engine_status` is never `isError` — answering
//! "reachable?" is its job. The four engine-reading/mutating tools use ACT-THEN-MAP: run the real
//! exchange and map [`StructureError::is_unreachable`] to the fail-fast result, no separate probe.

use std::path::Path;

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ListResourcesResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResult, Resource, ResourceContents,
    ServerCapabilities, ServerInfo,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};

use reuben_core::coordinator::{DiagnosticsReport, DocSource};
use reuben_core::introspect::{OperatorInfo, PatchBoundary};
use reuben_core::{schema, Arg, Registry, Report, SwapReport};
use reuben_native::resources::FsResolver;
use serde::{Deserialize, Serialize};

mod client;
mod engine;
pub use client::{DocumentSnapshot, StructureClient, StructureError, SwapOutcome};
pub use engine::{default_osc_addr, EngineChannel, EngineLink};

/// The eight-tool surface (ADR-0048 §1), in the ADR's roster order. The authority for the exact
/// spellings advertised over `tools/list`; the integration test asserts the wire surface matches.
pub const TOOL_NAMES: [&str; 8] = [
    "describe_operators",
    "describe_instrument",
    "validate",
    "send",
    "engine_status",
    "swap",
    "get_current_instrument",
    "get_diagnostics",
];

/// The actionable guidance an engine tool returns when the engine is unreachable (ADR-0044 §2):
/// the shim never spawns `reuben play`, so it names the fix instead.
pub const ENGINE_UNREACHABLE_GUIDANCE: &str =
    "The reuben engine is not reachable. Start it in another terminal with `reuben play`, then retry.";

/// The `reuben://schema/instrument` resource URI (ADR-0048 §7): the instrument JSON Schema,
/// generated live from the registry so it cannot drift. The authority for the URI advertised over
/// `resources/list`; the integration test asserts the wire surface matches.
pub const SCHEMA_RESOURCE_URI: &str = "reuben://schema/instrument";

/// The `reuben://guide/authoring` resource URI (ADR-0048 §7): `docs/agents/authoring.md`, read
/// from the checkout at request time (ADR-0051 §4).
pub const GUIDE_RESOURCE_URI: &str = "reuben://guide/authoring";

/// The MIME type advertised for [`SCHEMA_RESOURCE_URI`]. The body is a JSON Schema document; we
/// serve it as `application/json` — the universally parseable form MCP clients special-case — not
/// the narrower `application/schema+json` (ADR-0048 §7 fixes the URI, and leaves the MIME open).
pub const SCHEMA_RESOURCE_MIME: &str = "application/json";

/// The MIME type advertised for [`GUIDE_RESOURCE_URI`]: the authoring guide is CommonMark prose.
pub const GUIDE_RESOURCE_MIME: &str = "text/markdown";

/// The server `instructions` (ADR-0048 §7): the one-breath authoring gist. It carries the workflow
/// semantics — the document is durable truth; `send` to audition, doc-edit + `swap` to keep; start
/// `reuben play` first — and *points* at `reuben://guide/authoring` rather than restating the
/// contract (gist-and-point, ADR-0051 §4). The finalized prose is single-sourced by the
/// content-pass (#311); this is the real-but-refinable surface text.
const INSTRUCTIONS: &str = "reuben authoring sidecar. The instrument document is the durable \
     truth; keep it in sync with the sound. Start `reuben play` in another terminal first — the \
     engine tools (`send`, `swap`, `get_current_instrument`, `get_diagnostics`) fail fast until it \
     is reachable. The loop: `send` OSC to audition a change (ephemeral — clobbered at the next \
     swap), then edit the document and `swap` to make it durable. Read `reuben://guide/authoring` \
     for the type system, wiring rules, instrument format, and the authoring loop; \
     `reuben://schema/instrument` is the live instrument JSON Schema.";

/// Absolute path to the authoring guide (`docs/agents/authoring.md`), anchored at build time to
/// this crate's manifest dir (workspace-root-relative). The file is READ AT REQUEST TIME — never
/// `include_str!` — so a sidecar built yesterday still serves today's guide (ADR-0051 §4); only the
/// path is compile-time, valid in the checkout the sidecar is built and run from (the MVP persona,
/// ADR-0044). Matches the repo convention for locating workspace files (`CARGO_MANIFEST_DIR`).
const AUTHORING_GUIDE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/agents/authoring.md"
);

/// The instrument JSON Schema served at [`SCHEMA_RESOURCE_URI`], generated LIVE from the builtin
/// registry (ADR-0048 §7) so it can never drift from the operator set — and in the exact pretty
/// form committed to `crates/reuben-core/schema/instrument.schema.json`, so the served schema and
/// the committed copy stay byte-identical (guarded by `read_schema_resource_matches_committed`).
fn instrument_schema_json() -> String {
    schema::generate_pretty(&Registry::builtin())
}

/// The fail-fast result for an unreachable engine (ADR-0044 §2, ADR-0048 §3): `isError: true`
/// carrying the "start `reuben play`" guidance. `isError` tells the model the call could not do
/// its job and to act on the guidance rather than treat the payload as a deliverable.
pub fn engine_unreachable() -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(ENGINE_UNREACHABLE_GUIDANCE)])
}

/// Input for `describe_operators` (ADR-0048 §5): an optional `name` filter, mirroring
/// [`reuben_core::introspect::describe`]'s `Option<&str>`.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DescribeOperatorsParams {
    /// Restrict to one operator type; omit to list every registered operator.
    #[serde(default)]
    pub name: Option<String>,
}

/// Output for `describe_operators` (ADR-0048 §5): the operator set wrapped in `{ operators }`, so
/// the tool's `outputSchema` is an object (MCP requires an object root) whose one field is the
/// list mirroring [`reuben_core::introspect::describe`].
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DescribeOperatorsOutput {
    /// One entry per registered operator (or the single filtered one), in registry order.
    pub operators: Vec<OperatorInfo>,
}

/// Input for the read-only document tools `describe_instrument` and `validate` (ADR-0048 §2):
/// exactly one of `path` or `document`, with an optional `resolve_from` anchor for nested
/// references. The one-of and the resolver rooting are enforced by the tool body (#318).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DocumentParams {
    /// Path to an instrument file; the resolver roots at its directory (sibling-first, then the
    /// library root). Mutually exclusive with `document`.
    #[serde(default)]
    pub path: Option<String>,
    /// An inline instrument document. Mutually exclusive with `path`.
    #[serde(default)]
    pub document: Option<serde_json::Value>,
    /// Directory anchoring nested references for an inline `document` (defaults to the sidecar cwd).
    #[serde(default)]
    pub resolve_from: Option<String>,
}

/// One OSC message in a `send` batch (ADR-0048 §5): an address and its primitive args.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OscSendMessage {
    /// The full OSC address, e.g. `/voice1/cutoff`.
    pub address: String,
    /// The OSC arguments — numbers or strings.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

/// Input for `send` (ADR-0048 §5): a batch of **at least one** OSC message (the natural authoring
/// gesture is multi-control). The `length(min = 1)` puts `minItems: 1` in the advertised input
/// schema; the tool body rejects an empty batch too, for a client that skips schema validation.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendParams {
    /// The OSC messages to dispatch, in order (at least one).
    #[schemars(length(min = 1))]
    pub messages: Vec<OscSendMessage>,
}

/// Input for `swap` (ADR-0048 §5): a `path` (path-only — you can only install what exists on
/// disk) plus an optional `expect` content-hash guard (ADR-0046 §9).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SwapParams {
    /// Path to the instrument document to install.
    pub path: String,
    /// The content hash the client believes is installed; a mismatch rejects the swap (ADR-0046 §9).
    #[serde(default)]
    pub expect: Option<String>,
}

/// Output for `send` (ADR-0048 §5): how many OSC datagrams were dispatched. The count is
/// "left the socket", not "received" — UDP promises neither delivery nor application receipt.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SendOutput {
    /// The number of OSC messages dispatched to the engine.
    pub sent: usize,
}

/// The endpoints `engine_status` reports (ADR-0048 §5): the loopback structure channel and the
/// OSC control plane the sidecar talks to.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatusEndpoints {
    /// The structure channel address (`ping`/`swap`/`get_document`/`get_diagnostics`).
    pub structure: String,
    /// The OSC control endpoint `send` dispatches to.
    pub osc: String,
}

/// The sidecar identity `engine_status` reports (ADR-0048 §5): its own version and the instrument
/// `format_version` it supports (ADR-0048 §4 keeps `format_version` here, out of per-call reports).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SidecarInfo {
    /// The reuben-mcp crate version.
    pub version: String,
    /// The instrument document `format_version` this sidecar loads (`reuben_core::format`).
    pub format_version: u32,
}

/// Output for `engine_status` (ADR-0048 §5). **Never `isError`** for a dead engine — `reachable`
/// and the `guidance` (present only when unreachable) ARE the deliverable.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct EngineStatusOutput {
    /// Whether a live `reuben play` answered `ping` on the structure channel.
    pub reachable: bool,
    /// The structure and OSC endpoints this sidecar talks to.
    pub endpoints: StatusEndpoints,
    /// The sidecar's own identity.
    pub sidecar: SidecarInfo,
    /// The "start `reuben play`" guidance — present only when the engine is unreachable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<String>,
}

/// The `expect`-guard miss a [`SwapToolOutput`] carries (ADR-0046 §9): re-serialized field-for-
/// field from the channel's conflict so the model reconciles by re-reading, not by threading state.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SwapConflict {
    /// The hash the client asserted was installed.
    pub expected: String,
    /// The hash actually still playing — re-read via `get_current_instrument` to reconcile.
    pub actual: String,
}

/// Output for `swap` (ADR-0048 §§5,8): the shared [`SwapReport`] shape (ok, errors, warnings,
/// content_hash, and on success the diff summary) plus, on an `expect`-guard miss, `conflict`.
/// One `outputSchema` spans the install, validation-failure, and guard-miss cases; the flattened
/// [`SwapReport`] is the same serde type the structure channel serializes (ADR-0048 §8, no drift).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SwapToolOutput {
    /// The install report: `ok`, `errors`, `warnings`, the installed (or still-playing) content
    /// hash, and — on a successful install only — the diff summary.
    #[serde(flatten)]
    pub report: SwapReport,
    /// Present only on an `expect`-guard miss (ADR-0046 §9): nothing was installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<SwapConflict>,
}

impl SwapToolOutput {
    /// The engine processed the swap (success or `ok: false` load failure): the report is the whole
    /// story, no conflict.
    fn installed(report: SwapReport) -> Self {
        Self {
            report,
            conflict: None,
        }
    }

    /// The `expect` guard missed (ADR-0046 §9): nothing installed, so `ok: false` with no diff; the
    /// `content_hash` names what keeps playing (the conflict's `actual`), and `conflict` carries
    /// both hashes field-for-field for the model to reconcile.
    fn conflict(expected: String, actual: String) -> Self {
        Self {
            report: SwapReport {
                report: Report {
                    ok: false,
                    errors: vec![],
                    warnings: vec![],
                },
                content_hash: actual.clone(),
                diff: None,
            },
            conflict: Some(SwapConflict { expected, actual }),
        }
    }
}

/// Output for `get_current_instrument` (ADR-0048 §5): the Coordinator's canonical installed
/// document (raw JSON — the engine is the single validation authority) and its content hash.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct CurrentInstrumentOutput {
    /// The document the engine is currently playing.
    pub document: serde_json::Value,
    /// Its content hash — the token a later `swap`'s `expect` guard compares (ADR-0046 §9).
    pub content_hash: String,
}

/// The reuben MCP server: the eight-tool router plus the engine channel seam.
///
/// Pure tools (`describe_operators`, `describe_instrument`, `validate`) are always available;
/// the engine tools (`send`, `swap`, `get_current_instrument`, `get_diagnostics`) reach a
/// user-owned `reuben play` through [`EngineChannel`] and fail fast when it is unreachable.
/// `engine_status` answers "reachable?" and so is never itself an error (ADR-0048 §5).
pub struct ReubenServer {
    tool_router: ToolRouter<ReubenServer>,
    channel: Box<dyn EngineChannel>,
}

#[tool_router]
impl ReubenServer {
    /// A server backed by the shipping [`EngineLink`] on the shared default endpoints
    /// (`reuben_core::coordinator::DEFAULT_STRUCTURE_ADDR` and [`default_osc_addr`]): the engine
    /// tools reach a live `reuben play` over the real structure channel + OSC (ADR-0044 §2). The
    /// binary's composition root (`main`) injects the channel via [`with_channel`](Self::with_channel);
    /// this is the sensible default.
    pub fn new() -> Self {
        Self::with_channel(Box::new(EngineLink::default()))
    }

    /// A server with an explicit engine channel — the seam the unit tests drive with an in-memory
    /// fake to exercise every engine-tool branch (reachable, unreachable, conflict) without a
    /// socket.
    pub fn with_channel(channel: Box<dyn EngineChannel>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            channel,
        }
    }

    // --- Pure tools: always available (ADR-0044 §2) --------------------------------------------

    /// List the operator set (ADR-0048 §§1,5): delegates to [`reuben_core::introspect::describe`]
    /// and returns `{ operators: OperatorInfo[] }`, mirroring its `Option<&str>` filter exactly.
    /// Engine-free — always available (ADR-0044 §2). An unknown `name` is a can't-do-the-job error
    /// (ADR-0048 §5): there is no such operator to describe.
    #[tool(
        name = "describe_operators",
        description = "List the registered operators and their ports/params, optionally filtered by name.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<DescribeOperatorsOutput>()
            .expect("DescribeOperatorsOutput is an object schema")
    )]
    async fn describe_operators(
        &self,
        Parameters(params): Parameters<DescribeOperatorsParams>,
    ) -> Result<CallToolResult, McpError> {
        let registry = Registry::builtin();
        match reuben_core::introspect::describe(&registry, params.name.as_deref()) {
            Ok(operators) => {
                let summary = describe_operators_summary(&operators);
                structured_ok(&DescribeOperatorsOutput { operators }, summary)
            }
            // ADR-0048 §5: an unknown name is isError, not an empty deliverable.
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        }
    }

    /// Describe an instrument document's boundary as a host instrument will see it (ADR-0048 §5):
    /// resolves the one-of `path`/`document` (ADR-0048 §2), then delegates to
    /// [`reuben_core::introspect::describe_patch`] over a stat-only resolver and returns a
    /// [`PatchBoundary`]. Engine-free — always available (ADR-0044 §2). A document that fails to
    /// load has no boundary to describe, so it is isError pointing at `validate` (ADR-0048 §3).
    #[tool(
        name = "describe_instrument",
        description = "Describe an instrument document's boundary (inputs/outputs) as a host instrument sees it.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<PatchBoundary>()
            .expect("PatchBoundary is an object schema")
    )]
    async fn describe_instrument(
        &self,
        Parameters(params): Parameters<DocumentParams>,
    ) -> Result<CallToolResult, McpError> {
        let (json, resolver) = match load_document(&params) {
            Ok(loaded) => loaded,
            Err(err) => return Ok(err),
        };
        let registry = Registry::builtin();
        match reuben_core::introspect::describe_patch(&json, &registry, &resolver) {
            Ok(boundary) => {
                let summary = describe_instrument_summary(&boundary);
                structured_ok(&boundary, summary)
            }
            // ADR-0048 §3 corollary: no boundary to describe — direct the user to `validate`.
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "{message}\n\nThe document could not be loaded, so there is no boundary to \
                 describe. Run `validate` for the full report of errors and warnings."
            ))])),
        }
    }

    /// Validate an instrument document through the engine's own load + instantiate path (ADR-0048
    /// §5): resolves the one-of `path`/`document` (ADR-0048 §2), then delegates to
    /// [`reuben_core::introspect::validate`] over a stat-only resolver (no audio decode).
    /// Engine-free — always available (ADR-0044 §2). Error-layer discipline (ADR-0048 §3): a
    /// *failing* validation is an ordinary result carrying `{ ok: false, errors, warnings }` — the
    /// tool worked; only the can't-do-the-job cases (bad one-of, unreadable path) are isError.
    #[tool(
        name = "validate",
        description = "Validate an instrument document (load + instantiate); returns a report of errors and warnings.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<Report>()
            .expect("Report is an object schema")
    )]
    async fn validate(
        &self,
        Parameters(params): Parameters<DocumentParams>,
    ) -> Result<CallToolResult, McpError> {
        let (json, resolver) = match load_document(&params) {
            Ok(loaded) => loaded,
            Err(err) => return Ok(err),
        };
        let registry = Registry::builtin();
        let report = reuben_core::introspect::validate(&json, &registry, &resolver);
        let summary = validate_summary(&report);
        // Ordinary result even when `report.ok` is false: a report is the tool working (ADR-0048 §3).
        structured_ok(&report, summary)
    }

    // --- Engine tools: reach a user-owned `reuben play` through the channel seam ----------------

    /// Dispatch a batch of OSC control messages (ADR-0048 §5). Probe-first (UDP is silent about a
    /// dead port): every datagram is encoded and validated first, then the structure channel is
    /// pinged, then the batch is sent; an unreachable engine ⇒ `isError`.
    #[tool(
        name = "send",
        description = "Send a batch of OSC control messages to audition a change on the running engine. \
                       Ephemeral by design (ADR-0045 §5): these values live in render state only and are \
                       CLOBBERED at the next swap — fold any you want to keep into the instrument document \
                       and swap. The ack means the engine was reachable and the datagrams were dispatched; \
                       UDP promises neither delivery nor receipt. Fails fast if no engine is reachable.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<SendOutput>()
            .expect("SendOutput is an object schema")
    )]
    async fn send(
        &self,
        Parameters(params): Parameters<SendParams>,
    ) -> Result<CallToolResult, McpError> {
        // Reject an empty batch even though the input schema declares minItems:1 — belt-and-braces
        // against a client that skips schema validation (ADR-0048 §5: min 1).
        if params.messages.is_empty() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "`send` requires at least one OSC message.".to_string(),
            )]));
        }
        // Encode every datagram first: a bad address or argument is a can't-do-the-job error
        // (ADR-0048 §3), caught before any dispatch and even when the engine is down.
        let mut datagrams = Vec::with_capacity(params.messages.len());
        for (i, message) in params.messages.iter().enumerate() {
            let args = match osc_args_from_json(&message.args) {
                Ok(args) => args,
                Err(why) => {
                    return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                        "message {i} (`{}`) has an unsupported argument: {why}",
                        message.address
                    ))]))
                }
            };
            match reuben_native::osc::encode(&message.address, &args) {
                Ok(bytes) => datagrams.push(bytes),
                Err(why) => {
                    return Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                        "message {i} (`{}`) could not be encoded as OSC: {why}",
                        message.address
                    ))]))
                }
            }
        }
        // Probe-first (ADR-0048 §5): confirm liveness on the structure channel before dispatching,
        // since UDP would swallow a dead port silently. Any ping failure ⇒ the fail-fast result.
        if self.channel.ping().is_err() {
            return Ok(engine_unreachable());
        }
        match self.channel.send_osc(&datagrams) {
            Ok(sent) => structured_ok(
                &SendOutput { sent },
                format!("dispatched {sent} OSC message(s) to the engine"),
            ),
            Err(why) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "the engine is reachable but the OSC datagrams could not be dispatched: {why}"
            ))])),
        }
    }

    /// Liveness probe exposed as a tool (ADR-0048 §5). **Never `isError` for a dead engine** —
    /// answering "reachable?" is its job; `guidance` appears when the engine is down. Wraps the
    /// structure-channel `ping` (ADR-0046 §8) and reports the endpoints and sidecar identity.
    #[tool(
        name = "engine_status",
        description = "Report whether the reuben engine is reachable, with the structure/OSC endpoints and the \
                       sidecar version + supported instrument format_version. Never an error — a dead engine is \
                       reported as reachable:false with guidance to start it.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<EngineStatusOutput>()
            .expect("EngineStatusOutput is an object schema")
    )]
    async fn engine_status(&self) -> Result<CallToolResult, McpError> {
        let reachable = self.channel.ping().is_ok();
        let output = EngineStatusOutput {
            reachable,
            endpoints: StatusEndpoints {
                structure: self.channel.structure_endpoint(),
                osc: self.channel.osc_endpoint(),
            },
            sidecar: SidecarInfo {
                version: env!("CARGO_PKG_VERSION").to_string(),
                format_version: reuben_core::format::FORMAT_VERSION,
            },
            guidance: if reachable {
                None
            } else {
                Some(ENGINE_UNREACHABLE_GUIDANCE.to_string())
            },
        };
        let summary = if reachable {
            format!("engine reachable on {}", output.endpoints.structure)
        } else {
            "engine not reachable — start `reuben play`".to_string()
        };
        // NEVER isError (ADR-0048 §5): the reachable/guidance payload IS the deliverable.
        structured_ok(&output, summary)
    }

    /// Install an instrument document from disk (ADR-0048 §5). Path-only (ADR-0048 §2 — you can
    /// only install what exists on disk). Act-then-map: an unreachable engine ⇒ `isError`; an
    /// `ok: false` load report or an `expect` conflict is an ORDINARY result (the guard guarding,
    /// not the tool failing, ADR-0048 §3).
    #[tool(
        name = "swap",
        description = "Install an instrument document from disk as the playing engine (path-only). In M1 this is a \
                       restart-swap (ADR-0046 §10): a ~100ms silent gap, every node rebuilt cold, so the diff \
                       reports survived:0. Returns the validation report + content_hash + (on success) a diff \
                       summary; ok:false installs nothing and the old sound keeps playing. Pass `expect` (a \
                       content_hash) to guard against a stale swap — a mismatch returns a conflict, no install.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<SwapToolOutput>()
            .expect("SwapToolOutput is an object schema")
    )]
    async fn swap(
        &self,
        Parameters(params): Parameters<SwapParams>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .channel
            .swap(DocSource::Path(params.path), params.expect)
        {
            // An install report — success OR ok:false load failure — is an ORDINARY result: the
            // channel worked, the report is the deliverable (ADR-0048 §3).
            Ok(SwapOutcome::Installed(report)) => {
                let summary = swap_summary(&report);
                structured_ok(&SwapToolOutput::installed(report), summary)
            }
            // An `expect`-guard miss is the guard guarding, not the tool failing (ADR-0048 §3):
            // nothing installed, ordinary result carrying the conflict to reconcile (ADR-0046 §9).
            Ok(SwapOutcome::Conflict { expected, actual }) => {
                let summary = format!(
                    "swap rejected by the expect guard: the engine is playing {actual}, not the \
                     expected {expected} — re-read with get_current_instrument and reconcile"
                );
                structured_ok(&SwapToolOutput::conflict(expected, actual), summary)
            }
            Err(why) if why.is_unreachable() => Ok(engine_unreachable()),
            // The engine answered, but not with a domain report (a channel-level fault): the tool
            // could not do its job (ADR-0048 §3).
            Err(why) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "the swap could not be completed: {why}"
            ))])),
        }
    }

    /// Read the canonical installed document (ADR-0048 §5). Act-then-map: an unreachable engine ⇒
    /// `isError`. Forwards the structure-channel `get_document` and returns `{ document, content_hash }`.
    #[tool(
        name = "get_current_instrument",
        description = "Return the document the engine is currently playing, with its content hash (the token a \
                       later swap's `expect` guard compares). Fails fast if no engine is reachable.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<CurrentInstrumentOutput>()
            .expect("CurrentInstrumentOutput is an object schema")
    )]
    async fn get_current_instrument(&self) -> Result<CallToolResult, McpError> {
        match self.channel.get_document() {
            Ok(snapshot) => {
                let summary = format!(
                    "current instrument (content_hash {})",
                    snapshot.content_hash
                );
                let output = CurrentInstrumentOutput {
                    document: snapshot.document,
                    content_hash: snapshot.content_hash,
                };
                structured_ok(&output, summary)
            }
            Err(why) if why.is_unreachable() => Ok(engine_unreachable()),
            Err(why) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "could not read the current instrument: {why}"
            ))])),
        }
    }

    /// Read the engine diagnostics counters (ADR-0048 §5/§6). Act-then-map: an unreachable engine ⇒
    /// `isError`. Forwards the structure-channel `get_diagnostics` and returns the four counters.
    #[tool(
        name = "get_diagnostics",
        description = "Return the engine's running diagnostics counters since start: output_xruns (events) plus \
                       input_ring underruns/overruns/producer_drops (frames). Fails fast if no engine is reachable.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<DiagnosticsReport>()
            .expect("DiagnosticsReport is an object schema")
    )]
    async fn get_diagnostics(&self) -> Result<CallToolResult, McpError> {
        match self.channel.get_diagnostics() {
            Ok(report) => {
                let summary = format!(
                    "output_xruns={} input_ring_underruns={} input_ring_overruns={} \
                     input_ring_producer_drops={}",
                    report.output_xruns,
                    report.input_ring_underruns,
                    report.input_ring_overruns,
                    report.input_ring_producer_drops
                );
                structured_ok(&report, summary)
            }
            Err(why) if why.is_unreachable() => Ok(engine_unreachable()),
            Err(why) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "could not read the engine diagnostics: {why}"
            ))])),
        }
    }
}

impl Default for ReubenServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ReubenServer {
    /// Declare the `tools` and `resources` capabilities and the `instructions` field (ADR-0048
    /// §7). Providing `get_info` ourselves is what lets us add `resources` beside the tool
    /// router's `tools`; the `resources` capability is a **static** set — no subscribe/listChanged
    /// (ADR-0048 §7) — served by [`list_resources`](Self::list_resources) /
    /// [`read_resource`](Self::read_resource).
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.server_info = Implementation::new("reuben-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(INSTRUCTIONS.to_string());
        info
    }

    /// The static resource set (ADR-0048 §7): the live instrument schema and the authoring guide.
    /// No `subscribe`/`listChanged` — the capability builder declares neither, and this list never
    /// changes over a session, so there is no cursor to page.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(SCHEMA_RESOURCE_URI, "instrument schema")
                .with_title("Instrument JSON Schema")
                .with_description(
                    "The JSON Schema (draft 2020-12) for reuben instrument documents, generated \
                     live from the operator registry so it can never drift from the operator set.",
                )
                .with_mime_type(SCHEMA_RESOURCE_MIME),
            Resource::new(GUIDE_RESOURCE_URI, "authoring guide")
                .with_title("Instrument authoring guide")
                .with_description(
                    "docs/agents/authoring.md — the type system and wiring rules, the instrument \
                     format, addressing, and the try-then-commit authoring loop.",
                )
                .with_mime_type(GUIDE_RESOURCE_MIME),
        ]))
    }

    /// Read one static resource (ADR-0048 §7), served from the checkout at request time (ADR-0051
    /// §4): the schema is generated live from the registry, the guide is read from disk — never
    /// `include_str!`, so a sidecar built yesterday still serves today's content. An unknown URI is
    /// `resource_not_found`.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri = request.uri.as_str();
        let contents = match uri {
            SCHEMA_RESOURCE_URI => ResourceContents::text(instrument_schema_json(), uri)
                .with_mime_type(SCHEMA_RESOURCE_MIME),
            GUIDE_RESOURCE_URI => {
                let guide = std::fs::read_to_string(AUTHORING_GUIDE_PATH).map_err(|e| {
                    McpError::internal_error(
                        format!(
                            "failed to read the authoring guide at {AUTHORING_GUIDE_PATH}: {e}"
                        ),
                        None,
                    )
                })?;
                ResourceContents::text(guide, uri).with_mime_type(GUIDE_RESOURCE_MIME)
            }
            other => {
                return Err(McpError::resource_not_found(
                    format!(
                        "unknown resource `{other}`; this server serves {SCHEMA_RESOURCE_URI} and \
                         {GUIDE_RESOURCE_URI}"
                    ),
                    None,
                ))
            }
        };
        Ok(ReadResourceResult::new(vec![contents]))
    }
}

/// Convert a `send` message's JSON args into the flat primitive [`Arg`]s the OSC encoder packs
/// (ADR-0048 §5: args are `number | string`). An integer within `i32` range maps to `Arg::I32`,
/// any other number to `Arg::F32`, a string to `Arg::Str`; the engine re-types each against the
/// destination port at its boundary (dest-port-type-driven, [`reuben_native::osc::encode`]'s
/// contract). A non-number/non-string arg is a can't-do-the-job error naming the offending value.
fn osc_args_from_json(args: &[serde_json::Value]) -> Result<Vec<Arg>, String> {
    args.iter()
        .map(|value| match value {
            serde_json::Value::String(s) => Ok(Arg::Str(s.as_str().into())),
            serde_json::Value::Number(n) => {
                if let Some(i) = n.as_i64() {
                    if let Ok(i) = i32::try_from(i) {
                        return Ok(Arg::I32(i));
                    }
                }
                match n.as_f64() {
                    Some(f) => Ok(Arg::F32(f as f32)),
                    None => Err(format!("number {value} is out of range for OSC")),
                }
            }
            other => Err(format!(
                "{other} is not an OSC argument (expected a number or a string)"
            )),
        })
        .collect()
}

/// One-line human gloss of a swap outcome: what installed (with diff counts) or why nothing did.
fn swap_summary(report: &SwapReport) -> String {
    if report.report.ok {
        match &report.diff {
            Some(diff) => format!(
                "swapped (content_hash {}): {} survived, {} state-reset, {} added, {} removed",
                report.content_hash,
                diff.survived,
                diff.state_reset.len(),
                diff.added.len(),
                diff.removed.len()
            ),
            None => format!("swapped (content_hash {})", report.content_hash),
        }
    } else {
        format!(
            "swap rejected: {} error(s), {} warning(s) — nothing installed; {} keeps playing",
            report.report.errors.len(),
            report.report.warnings.len(),
            report.content_hash
        )
    }
}

/// Build an ordinary (non-error) result carrying BOTH a structured payload and a human-readable
/// text block (ADR-0048 §3). The structured content is what the model acts on; the text is the
/// gloss for a human reading the transcript. A serialization failure is a genuine internal fault,
/// so it surfaces as a protocol error rather than an `isError` deliverable.
fn structured_ok<T: Serialize>(value: &T, summary: String) -> Result<CallToolResult, McpError> {
    let structured = serde_json::to_value(value).map_err(|e| {
        McpError::internal_error(format!("failed to serialize tool output: {e}"), None)
    })?;
    let mut result = CallToolResult::structured(structured);
    // `structured` seeds `content` with a raw JSON dump; replace it with the human summary.
    result.content = vec![ContentBlock::text(summary)];
    Ok(result)
}

/// The isError result for a can't-do-the-job document-loading failure (ADR-0048 §3): an ambiguous
/// or missing one-of, or an unreadable path. `isError` tells the model to act on the guidance
/// rather than treat the payload as a deliverable.
fn cannot_load(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

/// Resolve a [`DocumentParams`] one-of into the instrument JSON plus a stat-only [`FsResolver`]
/// (ADR-0048 §2, ADR-0045 §4). Exactly one of `path`/`document` is required. `Ok` carries the JSON
/// text and a resolver rooted for nested references; `Err` is the ready-to-return `isError` result
/// for a bad one-of or an unreadable path — the can't-do-the-job cases (ADR-0048 §3).
fn load_document(params: &DocumentParams) -> Result<(String, FsResolver), CallToolResult> {
    match (&params.path, &params.document) {
        (Some(_), Some(_)) => Err(cannot_load(
            "provide exactly one of `path` or `document`, not both",
        )),
        (None, None) => Err(cannot_load("provide exactly one of `path` or `document`")),
        (Some(path), None) => {
            let path = Path::new(path);
            let json = std::fs::read_to_string(path).map_err(|e| {
                cannot_load(format!(
                    "could not read instrument path {}: {e}",
                    path.display()
                ))
            })?;
            // Root at the file's directory (sibling-first, library-root fallback), stat-only so
            // introspection reports port metadata without decoding any referenced audio.
            Ok((json, FsResolver::for_instrument(path).stat_only()))
        }
        (None, Some(document)) => {
            let json = serde_json::to_string(document).map_err(|e| {
                cannot_load(format!("inline `document` is not serializable JSON: {e}"))
            })?;
            // Anchor nested references at `resolve_from`, defaulting to the sidecar cwd; stat-only
            // for the same reason as the path branch (ADR-0048 §2).
            let base = params.resolve_from.as_deref().unwrap_or(".");
            Ok((json, FsResolver::new(base).stat_only()))
        }
    }
}

/// One-line human gloss of an operator listing: a single operator's port counts, or the roster.
fn describe_operators_summary(operators: &[OperatorInfo]) -> String {
    match operators {
        [one] => format!(
            "{}: {} input(s), {} output(s)",
            one.type_name,
            one.inputs.len(),
            one.outputs.len()
        ),
        many => {
            let names: Vec<&str> = many.iter().map(|o| o.type_name.as_str()).collect();
            format!("{} operators: {}", many.len(), names.join(", "))
        }
    }
}

/// One-line human gloss of a described instrument boundary.
fn describe_instrument_summary(boundary: &PatchBoundary) -> String {
    format!(
        "{}: {} boundary input(s), {} output(s)",
        boundary.instrument,
        boundary.inputs.len(),
        boundary.outputs.len()
    )
}

/// One-line human gloss of a validation report.
fn validate_summary(report: &Report) -> String {
    if report.ok {
        match report.warnings.len() {
            0 => "valid".to_string(),
            n => format!("valid ({n} warning(s))"),
        }
    } else {
        format!(
            "invalid: {} error(s), {} warning(s)",
            report.errors.len(),
            report.warnings.len()
        )
    }
}

/// Serve the MCP protocol over stdio until the client closes the connection (ADR-0044 §1). The
/// current_thread runtime is built by `main`; this is the async body it drives. `main` injects the
/// engine channel (the real [`EngineLink`] in the shipping binary), so the composition root stays
/// in `main` and tests can serve with a fake channel.
pub async fn serve_stdio(
    channel: Box<dyn EngineChannel>,
) -> Result<(), Box<dyn std::error::Error>> {
    let service = ReubenServer::with_channel(channel)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An in-memory [`EngineChannel`] the engine-tool unit tests inject to exercise every branch
    /// (reachable, unreachable, conflict, install) without a live `reuben play` or a socket. Each
    /// structure verb returns its configured outcome, or the fail-fast [`StructureError::Unreachable`]
    /// when none is configured — modelling a down engine per verb; `ping` reports `ping_ok`.
    struct FakeEngine {
        ping_ok: bool,
        swap: Option<SwapOutcome>,
        document: Option<DocumentSnapshot>,
        diagnostics: Option<DiagnosticsReport>,
    }

    impl FakeEngine {
        /// A reachable engine with no verb outcomes configured yet.
        fn reachable() -> Self {
            Self {
                ping_ok: true,
                swap: None,
                document: None,
                diagnostics: None,
            }
        }

        /// A down engine: `ping` fails and every structure verb is unreachable.
        fn unreachable() -> Self {
            Self {
                ping_ok: false,
                swap: None,
                document: None,
                diagnostics: None,
            }
        }

        fn with_swap(mut self, outcome: SwapOutcome) -> Self {
            self.swap = Some(outcome);
            self
        }

        fn with_document(mut self, snapshot: DocumentSnapshot) -> Self {
            self.document = Some(snapshot);
            self
        }

        fn with_diagnostics(mut self, report: DiagnosticsReport) -> Self {
            self.diagnostics = Some(report);
            self
        }
    }

    /// The fail-fast error a fake verb returns when its outcome is unconfigured — the same
    /// unreachable case the real client raises on a dead engine (ADR-0044 §2).
    fn down() -> StructureError {
        StructureError::Unreachable(ENGINE_UNREACHABLE_GUIDANCE.to_string())
    }

    impl EngineChannel for FakeEngine {
        fn ping(&self) -> Result<(), StructureError> {
            if self.ping_ok {
                Ok(())
            } else {
                Err(down())
            }
        }

        fn swap(
            &self,
            _source: DocSource,
            _expect: Option<String>,
        ) -> Result<SwapOutcome, StructureError> {
            self.swap.clone().ok_or_else(down)
        }

        fn get_document(&self) -> Result<DocumentSnapshot, StructureError> {
            self.document.clone().ok_or_else(down)
        }

        fn get_diagnostics(&self) -> Result<DiagnosticsReport, StructureError> {
            self.diagnostics.ok_or_else(down)
        }

        fn send_osc(&self, datagrams: &[Vec<u8>]) -> std::io::Result<usize> {
            Ok(datagrams.len())
        }

        fn structure_endpoint(&self) -> String {
            "127.0.0.1:9124".to_string()
        }

        fn osc_endpoint(&self) -> String {
            "127.0.0.1:9000".to_string()
        }
    }

    /// Drive an async tool body on the current-thread runtime (the only rt feature this crate enables).
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
            .block_on(future)
    }

    /// A server whose engine tools reach the given fake channel.
    fn server_with(fake: FakeEngine) -> ReubenServer {
        ReubenServer::with_channel(Box::new(fake))
    }

    /// Pull the first text block out of a result's content, for asserting on guidance text.
    fn first_text(result: &CallToolResult) -> String {
        result
            .content
            .iter()
            .find_map(|block| block.as_text().map(|t| t.text.clone()))
            .expect("result carries a text content block")
    }

    #[test]
    fn swap_result_serializes_report_hash_and_diff() {
        // ADR-0048 §§5,8: a successful swap serializes as the shared SwapReport shape —
        // { ok, errors, warnings, content_hash, diff } — with no `conflict` key. The tool output
        // FLATTENS the contract SwapReport, so the tool's structuredContent and the structure
        // channel's response are the same serde type and cannot drift.
        let report = SwapReport {
            report: Report {
                ok: true,
                errors: vec![],
                warnings: vec![],
            },
            content_hash: "00c0ffee".to_string(),
            diff: Some(reuben_core::DiffSummary {
                survived: 0,
                state_reset: vec!["/osc".to_string()],
                added: vec!["/delay".to_string()],
                removed: vec![],
            }),
        };
        let v = serde_json::to_value(SwapToolOutput::installed(report)).expect("serialize");
        assert_eq!(
            v,
            serde_json::json!({
                "ok": true,
                "errors": [],
                "warnings": [],
                "content_hash": "00c0ffee",
                "diff": {
                    "survived": 0,
                    "state_reset": ["/osc"],
                    "added": ["/delay"],
                    "removed": []
                }
            })
        );
        assert!(
            v.as_object().is_some_and(|o| !o.contains_key("conflict")),
            "a clean install omits the conflict key: {v}"
        );
    }

    #[test]
    fn engine_status_dead_engine_is_not_iserror_and_has_guidance() {
        // ADR-0048 §5: engine_status answers "reachable?", so it is NEVER isError — even on a dead
        // engine it reports the down state (reachable:false + guidance) as its deliverable, with the
        // endpoints and sidecar identity still filled in.
        let result = block_on(server_with(FakeEngine::unreachable()).engine_status())
            .expect("engine_status is infallible");
        assert_ne!(
            result.is_error,
            Some(true),
            "engine_status must not fail-fast on a dead engine: {result:?}"
        );
        let s = result
            .structured_content
            .as_ref()
            .expect("engine_status returns a structured payload");
        assert_eq!(
            s["reachable"].as_bool(),
            Some(false),
            "a dead engine reads as reachable:false: {s}"
        );
        assert!(
            s["guidance"]
                .as_str()
                .is_some_and(|g| g.contains("reuben play")),
            "the down-engine payload carries the `reuben play` guidance: {s}"
        );
        assert!(
            s["endpoints"]["structure"].is_string() && s["endpoints"]["osc"].is_string(),
            "the endpoints are reported even when down: {s}"
        );
        assert_eq!(
            s["sidecar"]["format_version"],
            serde_json::json!(reuben_core::format::FORMAT_VERSION),
            "the sidecar reports the supported instrument format_version: {s}"
        );
    }

    #[test]
    fn engine_status_reachable_reports_true_and_omits_guidance() {
        // The live branch: reachable:true and no guidance key (guidance appears only when down).
        let result = block_on(server_with(FakeEngine::reachable()).engine_status())
            .expect("engine_status is infallible");
        assert_ne!(result.is_error, Some(true));
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(s["reachable"].as_bool(), Some(true));
        assert!(
            s.as_object().is_some_and(|o| !o.contains_key("guidance")),
            "a reachable engine omits guidance: {s}"
        );
    }

    #[test]
    fn send_rejects_empty_messages() {
        // (a) The advertised input schema declares minItems:1 (ADR-0048 §5: min 1).
        let router_server = ReubenServer::new();
        let send = router_server
            .tool_router
            .list_all()
            .into_iter()
            .find(|t| t.name == "send")
            .expect("send is registered");
        let schema = serde_json::to_value(&send.input_schema).expect("input schema to value");
        assert_eq!(
            schema["properties"]["messages"]["minItems"],
            serde_json::json!(1),
            "send's input schema must require at least one message: {schema}"
        );
        // (b) The body rejects an empty batch as isError, for a client that skips schema validation.
        let result = block_on(
            server_with(FakeEngine::reachable()).send(Parameters(SendParams { messages: vec![] })),
        )
        .expect("send returns a result");
        assert_eq!(
            result.is_error,
            Some(true),
            "an empty batch must be isError: {result:?}"
        );
    }

    #[test]
    fn get_current_instrument_unreachable_is_iserror() {
        // ADR-0048 §3: a document read against a down engine is a can't-do-the-job isError carrying
        // the "start `reuben play`" guidance (act-then-map on get_document).
        let result = block_on(server_with(FakeEngine::unreachable()).get_current_instrument())
            .expect("returns a result");
        assert_eq!(
            result.is_error,
            Some(true),
            "an unreachable engine must be isError: {result:?}"
        );
        assert!(
            first_text(&result).contains("reuben play"),
            "the guidance must name the fix: {result:?}"
        );
    }

    #[test]
    fn swap_expect_mismatch_returns_conflict_no_install() {
        // ADR-0046 §9 / ADR-0048 §3: an expect-guard miss is an ORDINARY result (the guard
        // guarding, not the tool failing), NOT isError; nothing is installed (ok:false, no diff),
        // and the conflict names both hashes field-for-field so the model reconciles.
        let fake = FakeEngine::reachable().with_swap(SwapOutcome::Conflict {
            expected: "0badc0de".to_string(),
            actual: "00c0ffee".to_string(),
        });
        let result = block_on(server_with(fake).swap(Parameters(SwapParams {
            path: "instruments/pad.json".to_string(),
            expect: Some("0badc0de".to_string()),
        })))
        .expect("swap returns a result");
        assert_ne!(
            result.is_error,
            Some(true),
            "a conflict is a reconcilable outcome, not isError: {result:?}"
        );
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(
            s["ok"],
            serde_json::json!(false),
            "an expect miss installs nothing ⇒ ok:false: {s}"
        );
        assert!(
            s.as_object().is_some_and(|o| !o.contains_key("diff")),
            "a rejected swap has no diff to report: {s}"
        );
        assert_eq!(s["conflict"]["expected"], serde_json::json!("0badc0de"));
        assert_eq!(s["conflict"]["actual"], serde_json::json!("00c0ffee"));
    }

    #[test]
    fn m1_swap_diff_reports_survived_zero() {
        // ADR-0046 §10: M1 is restart-swap — every node rebuilt cold — so a successful swap's diff
        // reports survived:0 honestly. The tool relays the channel's report unchanged.
        let report = SwapReport {
            report: Report {
                ok: true,
                errors: vec![],
                warnings: vec![],
            },
            content_hash: "00c0ffee".to_string(),
            diff: Some(reuben_core::DiffSummary {
                survived: 0,
                state_reset: vec!["/osc".to_string()],
                added: vec![],
                removed: vec![],
            }),
        };
        let fake = FakeEngine::reachable().with_swap(SwapOutcome::Installed(report));
        let result = block_on(server_with(fake).swap(Parameters(SwapParams {
            path: "instruments/pad.json".to_string(),
            expect: None,
        })))
        .expect("swap returns a result");
        assert_ne!(
            result.is_error,
            Some(true),
            "a successful swap is an ordinary result: {result:?}"
        );
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(s["ok"], serde_json::json!(true));
        assert_eq!(
            s["diff"]["survived"],
            serde_json::json!(0),
            "M1 restart-swap reports survived:0: {s}"
        );
    }

    #[test]
    fn swap_unreachable_is_iserror() {
        // Act-then-map on the mutating verb: a down engine is the fail-fast isError.
        let result = block_on(server_with(FakeEngine::unreachable()).swap(Parameters(
            SwapParams {
                path: "instruments/pad.json".to_string(),
                expect: None,
            },
        )))
        .expect("swap returns a result");
        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("reuben play"));
    }

    #[test]
    fn send_dispatches_all_messages_when_reachable() {
        // A reachable send encodes every message and reports the count dispatched (ADR-0048 §5).
        let params = SendParams {
            messages: vec![
                OscSendMessage {
                    address: "/voice1/cutoff".to_string(),
                    args: vec![serde_json::json!(1200.0)],
                },
                OscSendMessage {
                    address: "/voice1/notes".to_string(),
                    args: vec![serde_json::json!(69), serde_json::json!(1)],
                },
            ],
        };
        let result = block_on(server_with(FakeEngine::reachable()).send(Parameters(params)))
            .expect("result");
        assert_ne!(
            result.is_error,
            Some(true),
            "a reachable send is an ordinary result: {result:?}"
        );
        assert_eq!(
            result
                .structured_content
                .as_ref()
                .expect("structured payload")["sent"],
            serde_json::json!(2)
        );
    }

    #[test]
    fn send_unreachable_is_iserror() {
        // Probe-first (ADR-0048 §5): datagrams encode fine, but a down engine fails the ping.
        let params = SendParams {
            messages: vec![OscSendMessage {
                address: "/voice1/cutoff".to_string(),
                args: vec![serde_json::json!(1.0)],
            }],
        };
        let result = block_on(server_with(FakeEngine::unreachable()).send(Parameters(params)))
            .expect("result");
        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("reuben play"));
    }

    #[test]
    fn send_rejects_a_non_scalar_argument() {
        // ADR-0048 §3/§5: args are number | string; a nested array is a can't-do-the-job error,
        // caught before any dispatch (and without needing the engine).
        let params = SendParams {
            messages: vec![OscSendMessage {
                address: "/voice1/cutoff".to_string(),
                args: vec![serde_json::json!([1, 2, 3])],
            }],
        };
        let result = block_on(server_with(FakeEngine::reachable()).send(Parameters(params)))
            .expect("result");
        assert_eq!(
            result.is_error,
            Some(true),
            "an unsupported argument must be isError: {result:?}"
        );
    }

    #[test]
    fn get_current_instrument_returns_document_and_hash() {
        let doc = serde_json::json!({ "format_version": 3, "instrument": "warm", "nodes": [] });
        let snapshot = DocumentSnapshot {
            document: doc.clone(),
            content_hash: "00c0ffee".to_string(),
        };
        let result = block_on(
            server_with(FakeEngine::reachable().with_document(snapshot)).get_current_instrument(),
        )
        .expect("result");
        assert_ne!(result.is_error, Some(true));
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(s["content_hash"], serde_json::json!("00c0ffee"));
        assert_eq!(s["document"]["instrument"], serde_json::json!("warm"));
    }

    #[test]
    fn get_diagnostics_returns_the_four_counters() {
        let report = DiagnosticsReport {
            output_xruns: 2,
            input_ring_underruns: 480,
            input_ring_overruns: 0,
            input_ring_producer_drops: 96,
        };
        let result = block_on(
            server_with(FakeEngine::reachable().with_diagnostics(report)).get_diagnostics(),
        )
        .expect("result");
        assert_ne!(result.is_error, Some(true));
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(s["output_xruns"], serde_json::json!(2));
        assert_eq!(s["input_ring_underruns"], serde_json::json!(480));
        assert_eq!(s["input_ring_producer_drops"], serde_json::json!(96));
    }

    #[test]
    fn get_diagnostics_unreachable_is_iserror() {
        let result =
            block_on(server_with(FakeEngine::unreachable()).get_diagnostics()).expect("result");
        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("reuben play"));
    }

    #[test]
    fn all_eight_tools_are_registered() {
        // The router advertises exactly the ADR-0048 §1 roster — the same surface the stdio
        // integration test asserts over the wire, checked here without spawning a process.
        let server = ReubenServer::new();
        let mut advertised: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        advertised.sort();

        let mut expected: Vec<String> = TOOL_NAMES.iter().map(|n| n.to_string()).collect();
        expected.sort();

        assert_eq!(
            advertised, expected,
            "the tool surface must be the ADR-0048 §1 roster"
        );
    }
}
