//! reuben-mcp — the per-conversation MCP stdio sidecar (ADR-0044).
//!
//! The MCP client spawns this shim over stdio; it hosts the pure introspection tools in-process
//! and reaches a user-owned `reuben play` for the engine tools. This crate is the FIRST workspace
//! member allowed an async runtime: rmcp + tokio live here and nowhere else, fenced out of every
//! other member so the play/CLI/web builds stay std-only (ADR-0044 §3/§5).
//!
//! # M1 skeleton scope (#313)
//!
//! This module stands up the server shape: a [`ServerHandler`] declaring the `tools` and
//! `resources` capabilities plus an `instructions` field, and a tool router with all **eight**
//! tools (ADR-0048 §1) registered as **stubs**. The tool bodies proper land with the tool ticket
//! (#318); the resource set and the final `instructions` prose land with the resources ticket
//! (#319). Downstream wiring the stubs reference:
//!
//! - pure tools → [`reuben_core::introspect`] (`describe`/`describe_patch`/`validate`) over a
//!   [`reuben_native::resources::FsResolver`], returning the [`reuben_core::Report`]/`PatchBoundary`
//!   contract types (already schema-derivable here via the `schemars` fence — proven by
//!   [`ReubenServer::validate`]).
//! - `send` → [`reuben_native::osc::encode`] over the OSC/UDP boundary.
//! - engine liveness → the structure-channel `ping` (ADR-0046 §8). The MCP client ticket (#315)
//!   fills the [`EngineProbe`] seam with the real [`PingProbe`] (connect + `ping` succeeds ⇒
//!   reachable); [`UnreachableProbe`] remains as the deterministic down-engine seam the unit tests
//!   drive to exercise the fail-fast path (ADR-0044 §2, ADR-0048 §3).

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

use reuben_core::introspect::{OperatorInfo, PatchBoundary};
use reuben_core::{schema, Registry, Report};
use reuben_native::resources::FsResolver;
use serde::{Deserialize, Serialize};

mod client;
pub use client::{DocumentSnapshot, PingProbe, StructureClient, StructureError, SwapOutcome};

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

/// The engine-liveness seam (ADR-0044 §2). The real probe is the structure-channel `ping`
/// (ADR-0046 §8), implemented by [`PingProbe`] in this crate's client module (#315). A tool that
/// needs the engine probes through this trait and fails fast when it reports the engine down; the
/// trait stays the seam so tests can drive both branches without a socket.
pub trait EngineProbe: Send + Sync {
    /// Whether a live `reuben play` is reachable over the structure channel right now.
    fn is_reachable(&self) -> bool;
}

/// A probe that always reports the engine unreachable. Since #315 landed the real [`PingProbe`],
/// this is no longer the default — it survives as the deterministic down-engine seam the unit
/// tests drive to exercise the fail-fast path without standing up a server.
pub struct UnreachableProbe;

impl EngineProbe for UnreachableProbe {
    fn is_reachable(&self) -> bool {
        false
    }
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

/// Input for `send` (ADR-0048 §5): a batch of at least one OSC message (the natural authoring
/// gesture is multi-control). Emptiness is rejected by the tool body (#318).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendParams {
    /// The OSC messages to dispatch, in order.
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

/// The reuben MCP server: the eight-tool router plus the engine-liveness seam.
///
/// Pure tools (`describe_operators`, `describe_instrument`, `validate`) are always available;
/// engine tools (`send`, `swap`, `get_current_instrument`, `get_diagnostics`) fail fast when
/// [`EngineProbe`] reports the engine down. `engine_status` answers "reachable?" and so is never
/// itself an error (ADR-0048 §5).
pub struct ReubenServer {
    tool_router: ToolRouter<ReubenServer>,
    probe: Box<dyn EngineProbe>,
}

#[tool_router]
impl ReubenServer {
    /// A server backed by the real [`PingProbe`] on the shared default structure address
    /// (`reuben_core::coordinator::DEFAULT_STRUCTURE_ADDR`): engine reachability is a live
    /// structure-channel `ping` (ADR-0044 §2). The binary's composition root (`main`) may inject a
    /// probe explicitly via [`with_probe`](Self::with_probe); this is the sensible default.
    pub fn new() -> Self {
        Self::with_probe(Box::new(PingProbe::default()))
    }

    /// A server with an explicit liveness probe — the seam #315 fills, and the seam the unit
    /// tests drive to exercise both the reachable and unreachable branches.
    pub fn with_probe(probe: Box<dyn EngineProbe>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            probe,
        }
    }

    /// Fail-fast guard for engine-dependent tools (ADR-0044 §2, ADR-0048 §3). `Ok(())` when the
    /// engine is reachable; otherwise the [`engine_unreachable`] result to return to the client.
    fn require_engine(&self) -> Result<(), CallToolResult> {
        if self.probe.is_reachable() {
            Ok(())
        } else {
            Err(engine_unreachable())
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

    // --- Engine tools: fail fast when the engine is unreachable (ADR-0048 §3) ------------------

    /// Dispatch a batch of OSC control messages (ADR-0048 §5). Probe-first: engine unreachable ⇒
    /// `isError`. Stub: #318 encodes via [`reuben_native::osc::encode`] and returns `{ sent: N }`.
    #[tool(
        name = "send",
        description = "Send a batch of OSC control messages to the running engine (ephemeral audition; clobbered at the next swap)."
    )]
    async fn send(
        &self,
        Parameters(_params): Parameters<SendParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(unreachable) = self.require_engine() {
            return Ok(unreachable);
        }
        Ok(stub("send"))
    }

    /// Liveness probe exposed as a tool (ADR-0048 §5). **Never `isError` for a dead engine** —
    /// answering "reachable?" is its job; `guidance` appears when the engine is down. #318 fills
    /// `endpoints` and `sidecar { version, format_version }` from the structure-channel `ping`.
    #[tool(
        name = "engine_status",
        description = "Report whether the reuben engine is reachable; returns guidance to start it when it is not."
    )]
    async fn engine_status(&self) -> Result<CallToolResult, McpError> {
        let reachable = self.probe.is_reachable();
        let guidance = if reachable {
            serde_json::Value::Null
        } else {
            serde_json::Value::String(ENGINE_UNREACHABLE_GUIDANCE.to_string())
        };
        Ok(CallToolResult::structured(serde_json::json!({
            "reachable": reachable,
            "guidance": guidance,
        })))
    }

    /// Install an instrument document (restart-swap in M1, ADR-0046 §10) (ADR-0048 §5). Engine
    /// unreachable ⇒ `isError`. Stub: #318 forwards over the structure channel and returns a
    /// [`reuben_core::SwapReport`].
    #[tool(
        name = "swap",
        description = "Install an instrument document as the playing engine (M1 restart-swap: ~100ms gap, every node cold)."
    )]
    async fn swap(
        &self,
        Parameters(_params): Parameters<SwapParams>,
    ) -> Result<CallToolResult, McpError> {
        if let Err(unreachable) = self.require_engine() {
            return Ok(unreachable);
        }
        Ok(stub("swap"))
    }

    /// Read the canonical installed document (ADR-0048 §5). Engine unreachable ⇒ `isError`. Stub:
    /// #318 forwards the structure-channel `get_document` and returns `{ document, content_hash }`.
    #[tool(
        name = "get_current_instrument",
        description = "Return the document the engine is currently playing, with its content hash."
    )]
    async fn get_current_instrument(&self) -> Result<CallToolResult, McpError> {
        if let Err(unreachable) = self.require_engine() {
            return Ok(unreachable);
        }
        Ok(stub("get_current_instrument"))
    }

    /// Read the engine diagnostics counters (ADR-0048 §5/§6). Engine unreachable ⇒ `isError`.
    /// Stub: #318 forwards the structure-channel `get_diagnostics` and returns the four counters.
    #[tool(
        name = "get_diagnostics",
        description = "Return the engine's running diagnostics counters (xruns and input-ring drops) since start."
    )]
    async fn get_diagnostics(&self) -> Result<CallToolResult, McpError> {
        if let Err(unreachable) = self.require_engine() {
            return Ok(unreachable);
        }
        Ok(stub("get_diagnostics"))
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

/// The placeholder result an engine-tool stub returns until #318 fills the body. An ordinary
/// (non-error) result: the stub "worked", it just has nothing real to report yet.
fn stub(tool: &str) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(format!(
        "{tool}: not yet implemented in the M1 skeleton (see #318)."
    ))])
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
/// engine-liveness probe (the real [`PingProbe`] in the shipping binary), so the composition root
/// stays in `main` and tests can serve with a fake probe.
pub async fn serve_stdio(probe: Box<dyn EngineProbe>) -> Result<(), Box<dyn std::error::Error>> {
    let service = ReubenServer::with_probe(probe)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A probe that reports the engine live — the branch #315's real structure-channel client
    /// takes when `ping` succeeds.
    struct ReachableProbe;
    impl EngineProbe for ReachableProbe {
        fn is_reachable(&self) -> bool {
            true
        }
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
    fn engine_unreachable_is_iserror() {
        // ADR-0044 §2 / ADR-0048 §3: an engine tool on a down engine returns isError:true carrying
        // the actionable "start `reuben play`" guidance — driven through the real seam.
        let server = ReubenServer::with_probe(Box::new(UnreachableProbe));
        let guard = server
            .require_engine()
            .expect_err("an unreachable engine must fail the guard");

        assert_eq!(
            guard.is_error,
            Some(true),
            "the guard result must be isError"
        );
        assert!(
            first_text(&guard).contains("reuben play"),
            "the guidance must name the fix: {guard:?}"
        );
    }

    #[test]
    fn engine_status_is_not_iserror_when_engine_unreachable() {
        // ADR-0048 §5: engine_status answers "reachable?", so it is NEVER isError — even on a dead
        // engine it reports the down state as its deliverable instead of fail-fasting like the
        // engine tools do. Driven through the same UnreachableProbe seam as
        // `engine_unreachable_is_iserror`; the tool is async, so drive it on a current-thread
        // runtime (the only rt feature this crate enables).
        let server = ReubenServer::with_probe(Box::new(UnreachableProbe));
        let result = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
            .block_on(server.engine_status())
            .expect("engine_status is infallible");

        assert_ne!(
            result.is_error,
            Some(true),
            "engine_status must not fail-fast on a dead engine: {result:?}"
        );

        let structured = result
            .structured_content
            .as_ref()
            .expect("engine_status returns a structured payload");
        assert_eq!(
            structured["reachable"].as_bool(),
            Some(false),
            "engine_status must report the engine unreachable: {structured}"
        );
        assert!(
            structured["guidance"]
                .as_str()
                .is_some_and(|g| g.contains("reuben play")),
            "the down-engine payload must carry the `reuben play` guidance: {structured}"
        );
    }

    #[test]
    fn reachable_engine_passes_the_guard() {
        // The other branch of the seam: a live engine lets the tool proceed to its real work.
        let server = ReubenServer::with_probe(Box::new(ReachableProbe));
        assert!(
            server.require_engine().is_ok(),
            "a reachable engine must pass the fail-fast guard"
        );
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
