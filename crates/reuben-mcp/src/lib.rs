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
//! - engine liveness → the structure-channel `ping` (ADR-0046 §8), owned by the MCP client ticket
//!   (#315). Until that lands, the [`EngineProbe`] seam is filled by [`UnreachableProbe`], so every
//!   engine tool deterministically exercises the fail-fast path (ADR-0044 §2, ADR-0048 §3).

use rmcp::handler::server::tool::ToolRouter;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};

use reuben_core::{Diag, Report};
use serde::Deserialize;

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

/// Placeholder server `instructions` (ADR-0048 §7). The one-paragraph authoring gist — the
/// document is truth; `send` to try, doc-edit + `swap` to keep; start `reuben play` first — is
/// single-sourced against the skills by the resources ticket (#319), which replaces this text.
const INSTRUCTIONS_PLACEHOLDER: &str =
    "reuben authoring sidecar (M1 skeleton). Placeholder instructions — the authoring workflow \
     text is filled in by the resources ticket (#319).";

/// The engine-liveness seam (ADR-0044 §2). The real probe is the structure-channel `ping`
/// (ADR-0046 §8), owned by the MCP client ticket **#315** — deliberately NOT implemented here, so
/// this ticket does not build the structure channel. A tool that needs the engine probes through
/// this trait and fails fast when it reports the engine down.
pub trait EngineProbe: Send + Sync {
    /// Whether a live `reuben play` is reachable over the structure channel right now.
    fn is_reachable(&self) -> bool;
}

/// The M1 placeholder probe: the engine is always reported unreachable, because the real
/// structure-channel client (#315) does not exist yet. Every engine tool therefore returns the
/// fail-fast "start `reuben play`" result until #315 lands.
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
    /// A server backed by the M1 [`UnreachableProbe`] (no structure-channel client yet, #315).
    pub fn new() -> Self {
        Self::with_probe(Box::new(UnreachableProbe))
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

    /// List the operator set (ADR-0048 §5). Stub: #318 delegates to
    /// [`reuben_core::introspect::describe`] and returns `{ operators: OperatorInfo[] }`.
    #[tool(
        name = "describe_operators",
        description = "List the registered operators and their ports/params, optionally filtered by name."
    )]
    async fn describe_operators(
        &self,
        Parameters(_params): Parameters<DescribeOperatorsParams>,
    ) -> Result<CallToolResult, McpError> {
        Ok(stub("describe_operators"))
    }

    /// Describe an instrument document's boundary as a host will see it (ADR-0048 §5). Stub: #318
    /// delegates to [`reuben_core::introspect::describe_patch`] and returns a `PatchBoundary`.
    #[tool(
        name = "describe_instrument",
        description = "Describe an instrument document's boundary (inputs/outputs) as a host instrument sees it."
    )]
    async fn describe_instrument(
        &self,
        Parameters(_params): Parameters<DocumentParams>,
    ) -> Result<CallToolResult, McpError> {
        Ok(stub("describe_instrument"))
    }

    /// Validate an instrument document through the engine's own load/instantiate path (ADR-0048
    /// §5). Stub returning an inert [`Report`]: it proves the reuben-core contract type is
    /// schema-derivable here (the `schemars` fence), so #318 gets the `outputSchema` for free by
    /// delegating to [`reuben_core::introspect::validate`].
    #[tool(
        name = "validate",
        description = "Validate an instrument document (load + instantiate); returns a report of errors and warnings."
    )]
    async fn validate(&self, Parameters(_params): Parameters<DocumentParams>) -> Json<Report> {
        Json(Report {
            ok: false,
            errors: vec![Diag {
                node: None,
                port: None,
                message: "validate is a stub in the M1 skeleton; the real load/instantiate path \
                          lands with #318 (reuben_core::introspect::validate)"
                    .to_string(),
            }],
            warnings: Vec::new(),
        })
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
    /// router's `tools`; the static resource set (`reuben://schema/instrument`,
    /// `reuben://guide/authoring`) and the final instruction prose are the resources ticket's
    /// (#319) — this skeleton only declares the surface.
    fn get_info(&self) -> ServerInfo {
        let mut info = ServerInfo::default();
        info.protocol_version = ProtocolVersion::LATEST;
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.server_info = Implementation::new("reuben-mcp", env!("CARGO_PKG_VERSION"));
        info.instructions = Some(INSTRUCTIONS_PLACEHOLDER.to_string());
        info
    }
}

/// The placeholder result a pure-tool stub returns until #318 fills the body. An ordinary
/// (non-error) result: the stub "worked", it just has nothing real to report yet.
fn stub(tool: &str) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(format!(
        "{tool}: not yet implemented in the M1 skeleton (see #318)."
    ))])
}

/// Serve the MCP protocol over stdio until the client closes the connection (ADR-0044 §1). The
/// current_thread runtime is built by `main`; this is the async body it drives.
pub async fn serve_stdio() -> Result<(), Box<dyn std::error::Error>> {
    let service = ReubenServer::new().serve(rmcp::transport::stdio()).await?;
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
