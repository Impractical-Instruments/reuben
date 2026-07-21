//! reuben-mcp — the per-conversation MCP stdio sidecar.
//!
//! The MCP client spawns this shim over stdio; it hosts the pure introspection tools in-process
//! and reaches a user-owned `reuben play` for the engine tools. This crate is the FIRST workspace
//! member allowed an async runtime: rmcp + tokio live here and nowhere else, fenced out of every
//! other member so the play/CLI/web builds stay std-only.
//!
//! # Tool surface
//!
//! A [`ServerHandler`] declaring the `tools` and `resources` capabilities plus an `instructions`
//! field, and a tool router with the full declared contract set (the
//! [`reuben_core::tools::CONTRACTS`] roster). The pure tools
//! (`describe_operators`/`describe_instrument`/`validate`, #316, plus `scaffold_instrument`, #158)
//! are engine-free — `describe`/`validate` descend to [`reuben_core::introspect`] over a
//! [`reuben_native::resources::FsResolver`], and `scaffold_instrument` mints a minimal valid
//! document by value ([`reuben_core::scaffold_instrument`]); the five engine
//! tools (`send`/`engine_status`/`swap`/`get_current_instrument`/`get_diagnostics`, #318) reach a
//! user-owned `reuben play` through the [`EngineLink`]'s two planes:
//!
//! - `send` → [`reuben_native::osc::encode`] over the OSC/UDP control path (probe-first liveness).
//! - `engine_status`/`swap`/`get_current_instrument`/`get_diagnostics` → the structure channel's
//!   four verbs, via [`StructureClient`] over an injectable [`StructureTransport`].
//!
//! Error-layer discipline: a failing validation or a rejected swap is an ORDINARY
//! result — the tool worked; `isError` is reserved for the can't-do-the-job cases (an unreachable
//! engine, a bad one-of, an unknown operator). `engine_status` is never `isError` — answering
//! "reachable?" is its job. The four engine-reading/mutating tools use ACT-THEN-MAP: run the real
//! exchange and map [`StructureError::is_unreachable`] to the fail-fast result, no separate probe.
//!
//! see rules: agent-mcp

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
use reuben_core::{Arg, Registry, Report, SwapReport};
use reuben_native::resources::FsResolver;
use serde::{Deserialize, Serialize};

mod client;
mod engine;
pub use client::{StructureClient, StructureError, StructureTransport, SwapOutcome, TcpTransport};
pub use engine::{default_osc_addr, EngineLink};
pub use reuben_core::coordinator::{Conflict, DocumentSnapshot};

/// The tool surface this door advertises, in roster order — the exact spellings
/// advertised over `tools/list`. Derived from the single-source [`reuben_core::tools::CONTRACTS`]
/// roster (#157) rather than a hand-typed literal, so the wire surface can only change by changing
/// the roster; the integration test asserts `tools/list` matches this same derivation.
pub fn tool_names() -> Vec<&'static str> {
    reuben_core::tools::names()
}

/// The actionable guidance an engine tool returns when the engine is unreachable:
/// the shim never spawns `reuben play`, so it names the fix instead.
pub const ENGINE_UNREACHABLE_GUIDANCE: &str =
    "The reuben engine is not reachable. Start it in another terminal with `reuben play`, then retry.";

/// The `reuben://guide/authoring` resource URI: the
/// authoring guide, `docs/agents/authoring.md`, read from the checkout at request time.
/// The authority for the URI advertised over `resources/list`; the integration
/// test asserts the wire surface matches. (The instrument-JSON-Schema resource this once served
/// beside was deleted outright.)
pub const GUIDE_RESOURCE_URI: &str = "reuben://guide/authoring";

/// The MIME type advertised for [`GUIDE_RESOURCE_URI`]: the authoring guide is CommonMark prose.
pub const GUIDE_RESOURCE_MIME: &str = "text/markdown";

/// The `reuben://guide/vocabulary` resource URI: the rendered intent→parameter vocabulary,
/// `docs/agents/vocabulary.md` (generated and staleness-tested against the registry by R5, #462),
/// read from the checkout at request time — the same posture as [`GUIDE_RESOURCE_URI`].
pub const VOCABULARY_RESOURCE_URI: &str = "reuben://guide/vocabulary";

/// The MIME type advertised for [`VOCABULARY_RESOURCE_URI`]: the rendered vocabulary is
/// CommonMark prose.
pub const VOCABULARY_RESOURCE_MIME: &str = "text/markdown";

/// The library-index resource URI: the generated
/// signature-line index over the available instrument set, `instruments/index.md` (generated and
/// staleness-tested against `instruments/` by R4, #461), read from the checkout at request time
/// — the same posture as [`GUIDE_RESOURCE_URI`]. No exact URI is mandated;
/// this lives in the same `guide/` namespace as [`GUIDE_RESOURCE_URI`] and
/// [`VOCABULARY_RESOURCE_URI`] — one namespace for all agent-read grounding, rather than minting
/// a second category for what is, from a client's view, just another pointed-at document.
pub const LIBRARY_INDEX_RESOURCE_URI: &str = "reuben://guide/library-index";

/// The MIME type advertised for [`LIBRARY_INDEX_RESOURCE_URI`]: the generated index is CommonMark
/// prose.
pub const LIBRARY_INDEX_RESOURCE_MIME: &str = "text/markdown";

/// The server `instructions`: the one-breath authoring gist. It carries the workflow
/// semantics — the document is durable truth; `send` to audition, doc-edit + `swap` to keep; start
/// `reuben play` first — and *points* at `reuben://guide/authoring` rather than restating the
/// contract (gist-and-point). It also points once each at the vocabulary and library-index
/// resources. The finalized prose is single-sourced by the content-pass (#311); this is the
/// real-but-refinable surface text.
const INSTRUCTIONS: &str = "reuben authoring sidecar. The instrument document is the durable \
     truth; keep it in sync with the sound. Start `reuben play` in another terminal first — the \
     engine tools (`send`, `swap`, `get_current_instrument`, `get_diagnostics`) fail fast until it \
     is reachable. The loop: `send` OSC to audition a change (ephemeral — clobbered at the next \
     swap), then edit the document and `swap` to make it durable. Creating an instrument from \
     scratch? Call `scaffold_instrument` for a guaranteed-valid starting document, then edit and \
     `swap` it. Read `reuben://guide/authoring` \
     for the type system, wiring rules, instrument format, and the authoring loop. Read \
     `reuben://guide/vocabulary` for the word→move table translating intent language (\"warmer\", \
     \"busier\", \"sadder\") into parameter moves. Read `reuben://guide/library-index` for the \
     available instruments to reuse by reference through a `subpatch` node.";

/// Default absolute path to the authoring guide (`docs/agents/authoring.md`), anchored at build
/// time to this crate's manifest dir (workspace-root-relative). The file is READ AT REQUEST TIME —
/// never `include_str!` — so a sidecar built yesterday still serves today's guide;
/// only the path is compile-time, valid in the checkout the sidecar is built and run from (the MVP
/// persona). Matches the repo convention for locating workspace files
/// (`CARGO_MANIFEST_DIR`). A deploy that runs the sidecar *outside* that checkout overrides it with
/// [`AUTHORING_GUIDE_ENV`] (see [`ResourceEntry::resolve_path`]).
const AUTHORING_GUIDE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/agents/authoring.md"
);

/// Default absolute path to the rendered vocabulary (`docs/agents/vocabulary.md`), anchored at
/// build time to this crate's manifest dir — the same checkout-relative, read-at-request-time
/// posture as [`AUTHORING_GUIDE_PATH`]. Overridden for a non-checkout deploy by
/// [`VOCABULARY_ENV`] (see [`ResourceEntry::resolve_path`]).
const VOCABULARY_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/agents/vocabulary.md"
);

/// Default absolute path to the generated library index (`instruments/index.md`), anchored at
/// build time to this crate's manifest dir — the same checkout-relative, read-at-request-time
/// posture as [`AUTHORING_GUIDE_PATH`]. Overridden for a non-checkout deploy by
/// [`LIBRARY_INDEX_ENV`] (see [`ResourceEntry::resolve_path`]).
const LIBRARY_INDEX_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../instruments/index.md");

/// Env override for the authoring-guide path: point the `reuben://guide/authoring` resource at an
/// explicit file for a **non-checkout deploy** — a shipped sidecar binary whose compile-time
/// [`AUTHORING_GUIDE_PATH`] points into a build checkout that need not exist where it runs.
/// Unset keeps the compile-time default. Mirrors the `REUBEN_INSTRUMENT_ROOT`
/// convention (a `REUBEN_*` path override resolved from the environment).
pub const AUTHORING_GUIDE_ENV: &str = "REUBEN_AUTHORING_GUIDE";

/// Env override for the vocabulary path, mirroring [`AUTHORING_GUIDE_ENV`] for
/// [`VOCABULARY_RESOURCE_URI`]: point a non-checkout deploy at an explicit file.
pub const VOCABULARY_ENV: &str = "REUBEN_VOCABULARY";

/// Env override for the library-index path, mirroring [`AUTHORING_GUIDE_ENV`] for
/// [`LIBRARY_INDEX_RESOURCE_URI`]: point a non-checkout deploy at an explicit file.
pub const LIBRARY_INDEX_ENV: &str = "REUBEN_LIBRARY_INDEX";

/// Resolve a checkout-relative resource path: the given env override when set, else `default`.
/// Pure over an already-read env value so it is unit-testable without mutating (and racing on)
/// the process environment — the shared machinery behind [`ResourceEntry::resolve_path`].
fn resolve_checkout_path(
    env_override: Option<std::ffi::OsString>,
    default: &str,
) -> std::path::PathBuf {
    env_override
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(default))
}

/// One served MCP resource: the wire facts a client
/// sees (`uri`/`name`/`title`/`description`/`mime`) plus the serve mechanics (`env` override,
/// compile-time `default_path`, and the `noun` for the read-failure message). All three resources
/// are structurally identical — a static markdown file read from the checkout at request time
/// (never `include_str!`), env-overridable per resource — so the surface is one table
/// over the existing consts and one generic serve path, not N hand-spelled arms. The consts stay the
/// single source of the wire URIs/MIMEs/env-vars/paths; this table is built *from* them. Only the six
/// `*_RESOURCE_URI`/`*_RESOURCE_MIME` consts are external API (referenced by
/// `tests/stdio_resources.rs`, so they can't be inlined away); the three `*_ENV` consts are `pub` but
/// used only here, and the three `*_PATH` consts are private.
struct ResourceEntry {
    /// The URI advertised over `resources/list` and matched on `resources/read`.
    uri: &'static str,
    /// The short resource name.
    name: &'static str,
    /// The human title.
    title: &'static str,
    /// The one-paragraph description.
    description: &'static str,
    /// The MIME type (every resource is `text/markdown`).
    mime: &'static str,
    /// The `REUBEN_*` env var that overrides [`ResourceEntry::default_path`] for a non-checkout
    /// deploy.
    env: &'static str,
    /// The compile-time, checkout-relative default path the resource is read from when `env` is
    /// unset.
    default_path: &'static str,
    /// The noun used in the read-failure message (`failed to read the {noun} at {path}: {e}`);
    /// distinct from `name` where the two diverge (e.g. `intent vocabulary` vs `vocabulary`), so
    /// the error prose is preserved bit-for-bit.
    noun: &'static str,
}

impl ResourceEntry {
    /// Resolves this entry's on-disk path, reading the `self.env` override then falling back to
    /// `self.default_path`. See [`resolve_checkout_path`].
    fn resolve_path(&self) -> std::path::PathBuf {
        resolve_checkout_path(std::env::var_os(self.env), self.default_path)
    }

    /// Read this resource's content from disk at request time into a
    /// [`ResourceContents`] carrying its URI and MIME. A read failure is a genuine internal fault
    /// (the checkout path is missing or unreadable), surfaced as a protocol error naming the noun
    /// and path.
    fn read_contents(&self) -> Result<ResourceContents, McpError> {
        let path = self.resolve_path();
        let text = std::fs::read_to_string(&path).map_err(|e| {
            McpError::internal_error(
                format!(
                    "failed to read the {} at {}: {e}",
                    self.noun,
                    path.display()
                ),
                None,
            )
        })?;
        Ok(ResourceContents::text(text, self.uri).with_mime_type(self.mime))
    }
}

/// The static resource roster, in wire order: the
/// authoring guide, the intent vocabulary, and the library index — the single source
/// [`list_resources`](ReubenServer::list_resources) and [`read_resource`](ReubenServer::read_resource)
/// both drive. Adding a resource is one row here — plus the four consts it references (three `pub`:
/// `*_RESOURCE_URI`/`*_RESOURCE_MIME`/`*_ENV`; the `*_PATH` default is private), the hardcoded
/// 3-item wording in the `served_resource_uris_reads_as_an_oxford_list` test, and one line in
/// `tests/stdio_resources.rs`'s deliberate exact-set guard.
const RESOURCES: &[ResourceEntry] = &[
    ResourceEntry {
        uri: GUIDE_RESOURCE_URI,
        name: "authoring guide",
        title: "Instrument authoring guide",
        description: "docs/agents/authoring.md — the type system and wiring rules, the instrument \
             format, addressing, and the try-then-commit authoring loop.",
        mime: GUIDE_RESOURCE_MIME,
        env: AUTHORING_GUIDE_ENV,
        default_path: AUTHORING_GUIDE_PATH,
        noun: "authoring guide",
    },
    ResourceEntry {
        uri: VOCABULARY_RESOURCE_URI,
        name: "intent vocabulary",
        title: "Intent → parameter vocabulary",
        description: "docs/agents/vocabulary.md — the word→move table translating intent \
             language (\"warmer\", \"busier\", \"sadder\") into parameter moves, plus \
             the edge-conduct preamble and the direction-only fallback block.",
        mime: VOCABULARY_RESOURCE_MIME,
        env: VOCABULARY_ENV,
        default_path: VOCABULARY_PATH,
        noun: "vocabulary",
    },
    ResourceEntry {
        uri: LIBRARY_INDEX_RESOURCE_URI,
        name: "library index",
        title: "Instrument library index",
        description: "instruments/index.md — the generated signature-line index over the \
             available instrument set (name, recipe-role line, face) for selecting a \
             `subpatch` reference; trusted for selection only.",
        mime: LIBRARY_INDEX_RESOURCE_MIME,
        env: LIBRARY_INDEX_ENV,
        default_path: LIBRARY_INDEX_PATH,
        noun: "library index",
    },
];

/// The served resource URIs as an English list — single-sourced from [`RESOURCES`], joined with an
/// Oxford comma for three or more (`"X, Y, and Z"`) so the unknown-resource guidance names every row
/// with the pre-refactor grammar. Two items read `"X and Y"`; one reads `"X"`.
fn served_resource_uris() -> String {
    let uris: Vec<&str> = RESOURCES.iter().map(|r| r.uri).collect();
    match uris.as_slice() {
        [] => String::new(),
        [only] => only.to_string(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

/// The fail-fast result for an unreachable engine: `isError: true`
/// carrying the "start `reuben play`" guidance. `isError` tells the model the call could not do
/// its job and to act on the guidance rather than treat the payload as a deliverable.
pub fn engine_unreachable() -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(ENGINE_UNREACHABLE_GUIDANCE)])
}

/// Input for `describe_operators`: an optional `name` filter, mirroring
/// [`reuben_core::introspect::describe`]'s `Option<&str>`, plus the `compact` mode switch.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DescribeOperatorsParams {
    /// Restrict to one operator type; omit to list every registered operator.
    #[serde(default)]
    pub name: Option<String>,
    /// Compact mode: one generated signature line per operator instead of full
    /// port objects — the same registry truth, projected for grounding budgets. Default false.
    #[serde(default)]
    pub compact: bool,
}

/// Output for `describe_operators`: the operator set under an object root (MCP
/// requires one), in exactly one of the verb's two projections of the same registry truth
/// — `operators` (full port objects, the default) or `signatures` (the compact
/// mode), keyed by the `compact` param. Both mirror [`reuben_core::introspect::describe`] /
/// [`describe_compact`](reuben_core::introspect::describe_compact).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct DescribeOperatorsOutput {
    /// Full mode: one entry per registered operator (or the single filtered one), in registry
    /// order. Absent in compact mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operators: Option<Vec<OperatorInfo>>,
    /// Compact mode: one generated signature line per operator —
    /// `name(inputs; config: constants; res: resource-slots) -> outputs`. Absent in full mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signatures: Option<Vec<String>>,
}

/// Input for `scaffold_instrument` (#158, closes #146): an optional `name` for the minted
/// document. Omit it for the default (`untitled`, [`reuben_core::SCAFFOLD_DEFAULT_NAME`]).
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ScaffoldInstrumentParams {
    /// The `instrument` name of the scaffolded document; omit for the default (`untitled`).
    #[serde(default)]
    pub name: Option<String>,
}

/// Output for `scaffold_instrument` (#158): the guaranteed-valid minimal instrument document,
/// returned **by value** under an object root (MCP requires one). The model edits this seed and
/// swaps it — first-creation as reshape-from-template, not authoring from a blank file (the
/// document travels by value; writing it to disk stays native-only).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct ScaffoldInstrumentOutput {
    /// A minimal valid document: `{ "format_version": 3, "instrument": <name>, "nodes": [] }`.
    pub document: serde_json::Value,
}

/// Input for the read-only document tools `describe_instrument` and `validate`:
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

/// One OSC message in a `send` batch: an address and its primitive args.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct OscSendMessage {
    /// The full OSC address, e.g. `/voice1/cutoff`.
    pub address: String,
    /// The OSC arguments — numbers or strings.
    #[serde(default)]
    pub args: Vec<serde_json::Value>,
}

/// Input for `send`: a batch of **at least one** OSC message (the natural authoring
/// gesture is multi-control). The `length(min = 1)` puts `minItems: 1` in the advertised input
/// schema; the tool body rejects an empty batch too, for a client that skips schema validation.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SendParams {
    /// The OSC messages to dispatch, in order (at least one).
    #[schemars(length(min = 1))]
    pub messages: Vec<OscSendMessage>,
}

/// Input for `swap`: a `path` (path-only — you can only install what exists on
/// disk) plus an optional `expect` content-hash guard.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SwapParams {
    /// Path to the instrument document to install.
    pub path: String,
    /// The content hash the client believes is installed; a mismatch rejects the swap.
    #[serde(default)]
    pub expect: Option<String>,
}

/// Output for `send`: how many OSC datagrams were dispatched. The count is
/// "left the socket", not "received" — UDP promises neither delivery nor application receipt.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SendOutput {
    /// The number of OSC messages dispatched to the engine.
    pub sent: usize,
}

/// The endpoints `engine_status` reports: the loopback structure channel and the
/// OSC control plane the sidecar talks to.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct StatusEndpoints {
    /// The structure channel address (`ping`/`swap`/`get_document`/`get_diagnostics`).
    pub structure: String,
    /// The OSC control endpoint `send` dispatches to.
    pub osc: String,
}

/// The sidecar identity `engine_status` reports: its own version and the instrument
/// `format_version` it supports (kept here, out of per-call reports).
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SidecarInfo {
    /// The reuben-mcp crate version.
    pub version: String,
    /// The instrument document `format_version` this sidecar loads (`reuben_core::format`).
    pub format_version: u32,
}

/// Output for `engine_status`. **Never `isError`** for a dead engine — `reachable`
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

/// Output for `swap`: the shared [`SwapReport`] shape (ok, errors, warnings,
/// content_hash, and on success the diff summary) plus, on an `expect`-guard miss, `conflict`.
/// One `outputSchema` spans the install, validation-failure, and guard-miss cases; both the
/// flattened [`SwapReport`] and the [`Conflict`] are the same serde types the structure channel
/// serializes, so the tool shape and the wire shape cannot drift.
#[derive(Debug, Serialize, schemars::JsonSchema)]
pub struct SwapToolOutput {
    /// The install report: `ok`, `errors`, `warnings`, the installed (or still-playing) content
    /// hash, and — on a successful install only — the diff summary.
    #[serde(flatten)]
    pub report: SwapReport,
    /// Present only on an `expect`-guard miss: nothing was installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conflict: Option<Conflict>,
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

    /// The `expect` guard missed: nothing installed. The report is
    /// [`SwapReport::rejected`] — which owns the "`content_hash` names what keeps playing"
    /// contract — and the channel's own [`Conflict`] rides along verbatim for the model to
    /// reconcile against.
    fn conflict(conflict: Conflict) -> Self {
        Self {
            report: SwapReport::rejected(conflict.actual.clone()),
            conflict: Some(conflict),
        }
    }
}

/// The reuben MCP server: the declared-roster tool router plus the engine link.
///
/// Pure tools (`describe_operators`, `describe_instrument`, `validate`) are always available;
/// the engine tools (`send`, `swap`, `get_current_instrument`, `get_diagnostics`) reach a
/// user-owned `reuben play` through [`EngineLink`] and fail fast when it is unreachable.
/// `engine_status` answers "reachable?" and so is never itself an error.
pub struct ReubenServer {
    tool_router: ToolRouter<ReubenServer>,
    engine: EngineLink,
}

#[tool_router]
impl ReubenServer {
    /// A server backed by an [`EngineLink`] on the shared default endpoints
    /// (`reuben_core::coordinator::DEFAULT_STRUCTURE_ADDR` and [`default_osc_addr`]): the engine
    /// tools reach a live `reuben play` over the real structure channel + OSC. The
    /// binary's composition root (`main`) injects the link via [`with_engine`](Self::with_engine);
    /// this is the sensible default.
    pub fn new() -> Self {
        Self::with_engine(EngineLink::default())
    }

    /// A server with an explicit engine link — the injection point for tests, which pair a fake
    /// structure transport with a UDP socket they bound themselves.
    pub fn with_engine(engine: EngineLink) -> Self {
        Self {
            tool_router: Self::tool_router(),
            engine,
        }
    }

    // --- Pure tools: always available -----------------------------------------------------------

    /// List the operator set: delegates to [`reuben_core::introspect::describe`]
    /// (or its compact signature-line mode when `compact` is set) and returns
    /// `{ operators }` / `{ signatures }`, mirroring the `Option<&str>` filter exactly.
    /// Engine-free — always available. An unknown `name` is a can't-do-the-job error:
    /// there is no such operator to describe.
    #[tool(
        name = "describe_operators",
        description = "List the registered operators and their ports/params, optionally filtered by name. \
                       Set compact:true for one generated signature line per operator — \
                       name(inputs; config: constants; res: resource-slots) -> outputs, each port as \
                       name:kind with enum [variants], unit, exp for an exponential curve, lo..hi, =default \
                       — instead of full port objects; the full mode stays the zoom for port detail.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<DescribeOperatorsOutput>()
            .expect("DescribeOperatorsOutput is an object schema")
    )]
    async fn describe_operators(
        &self,
        Parameters(params): Parameters<DescribeOperatorsParams>,
    ) -> Result<CallToolResult, McpError> {
        let registry = Registry::builtin();
        if params.compact {
            return match reuben_core::introspect::describe_compact(
                &registry,
                params.name.as_deref(),
            ) {
                Ok(signatures) => {
                    let summary = format!("{} operator signature(s) (compact)", signatures.len());
                    structured_ok(
                        &DescribeOperatorsOutput {
                            operators: None,
                            signatures: Some(signatures),
                        },
                        summary,
                    )
                }
                // An unknown name is isError, not an empty deliverable.
                Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
            };
        }
        match reuben_core::introspect::describe(&registry, params.name.as_deref()) {
            Ok(operators) => {
                let summary = describe_operators_summary(&operators);
                structured_ok(
                    &DescribeOperatorsOutput {
                        operators: Some(operators),
                        signatures: None,
                    },
                    summary,
                )
            }
            // An unknown name is isError, not an empty deliverable.
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(message)])),
        }
    }

    /// Describe an instrument document's boundary as a host instrument will see it:
    /// resolves the one-of `path`/`document`, then delegates to
    /// [`reuben_core::introspect::describe_patch`] over a stat-only resolver and returns a
    /// [`PatchBoundary`]. Engine-free — always available. A document that fails to
    /// load has no boundary to describe, so it is isError pointing at `validate`.
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
            // No boundary to describe — direct the user to `validate`.
            Err(message) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "{message}\n\nThe document could not be loaded, so there is no boundary to \
                 describe. Run `validate` for the full report of errors and warnings."
            ))])),
        }
    }

    /// Validate an instrument document through the engine's own load + instantiate path:
    /// resolves the one-of `path`/`document`, then delegates to
    /// [`reuben_core::introspect::validate`] over a stat-only resolver (no audio decode).
    /// Engine-free — always available. Error-layer discipline: a
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
        // Ordinary result even when `report.ok` is false: a report is the tool working.
        structured_ok(&report, summary)
    }

    /// Scaffold a guaranteed-valid minimal instrument document (#158, closes #146) — the
    /// first-creation start move. Authoring a top-level document from scratch stalls because the
    /// required top-level `instrument` name is easy to omit and `validate` then rejects the
    /// document; handing back a valid seed turns first-creation into the reshape-from-template path
    /// that already works (edit this document, then `swap` it). Engine-free — always available,
    /// read-only, and returns the document **by value**.
    #[tool(
        name = "scaffold_instrument",
        description = "Return a guaranteed-valid minimal instrument document to edit then swap — the start move for creating an instrument from scratch.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<ScaffoldInstrumentOutput>()
            .expect("ScaffoldInstrumentOutput is an object schema")
    )]
    async fn scaffold_instrument(
        &self,
        Parameters(params): Parameters<ScaffoldInstrumentParams>,
    ) -> Result<CallToolResult, McpError> {
        let document = reuben_core::scaffold_instrument(params.name.as_deref());
        let name = document["instrument"].as_str().unwrap_or("untitled");
        let summary = format!(
            "scaffolded minimal instrument {name:?} — edit its `nodes`/`interface`, then swap it"
        );
        structured_ok(&ScaffoldInstrumentOutput { document }, summary)
    }

    // --- Engine tools: reach a user-owned `reuben play` through the channel seam ----------------

    /// Dispatch a batch of OSC control messages. Probe-first (UDP is silent about a
    /// dead port): every datagram is encoded and validated first, then the structure channel is
    /// pinged, then the batch is sent; an unreachable engine ⇒ `isError`.
    #[tool(
        name = "send",
        description = "Send a batch of OSC control messages to audition a change on the running engine. \
                       Ephemeral by design: these values live in render state only and are \
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
        // against a client that skips schema validation (the schema declares min 1).
        if params.messages.is_empty() {
            return Ok(CallToolResult::error(vec![ContentBlock::text(
                "`send` requires at least one OSC message.".to_string(),
            )]));
        }
        // Encode every datagram first: a bad address or argument is a can't-do-the-job error,
        // caught before any dispatch and even when the engine is down.
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
        // Probe-first: confirm liveness on the structure channel before dispatching,
        // since UDP would swallow a dead port silently. Any ping failure ⇒ the fail-fast result.
        if self.engine.structure().ping().is_err() {
            return Ok(engine_unreachable());
        }
        match self.engine.send_osc(&datagrams) {
            Ok(sent) => structured_ok(
                &SendOutput { sent },
                format!("dispatched {sent} OSC message(s) to the engine"),
            ),
            Err(why) => Ok(CallToolResult::error(vec![ContentBlock::text(format!(
                "the engine is reachable but the OSC datagrams could not be dispatched: {why}"
            ))])),
        }
    }

    /// Liveness probe exposed as a tool. **Never `isError` for a dead engine** —
    /// answering "reachable?" is its job; `guidance` appears when the engine is down. Wraps the
    /// structure-channel `ping` and reports the endpoints and sidecar identity.
    #[tool(
        name = "engine_status",
        description = "Report whether the reuben engine is reachable, with the structure/OSC endpoints and the \
                       sidecar version + supported instrument format_version. Never an error — a dead engine is \
                       reported as reachable:false with guidance to start it.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<EngineStatusOutput>()
            .expect("EngineStatusOutput is an object schema")
    )]
    async fn engine_status(&self) -> Result<CallToolResult, McpError> {
        let reachable = self.engine.structure().ping().is_ok();
        let output = EngineStatusOutput {
            reachable,
            endpoints: StatusEndpoints {
                structure: self.engine.structure_endpoint(),
                osc: self.engine.osc_endpoint(),
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
        // NEVER isError: the reachable/guidance payload IS the deliverable.
        structured_ok(&output, summary)
    }

    /// Install an instrument document from disk. Path-only (you can
    /// only install what exists on disk). Act-then-map: an unreachable engine ⇒ `isError`; an
    /// `ok: false` load report or an `expect` conflict is an ORDINARY result (the guard guarding,
    /// not the tool failing).
    #[tool(
        name = "swap",
        description = "Install an instrument document from disk as the playing engine (path-only). A gapless \
                       mailbox swap: the new Engine is built and validated off-thread, then \
                       installed under a ~20ms master-gain duck — no silent gap. A node at the same \
                       address with the same operator type survives with its live state, so the diff summary \
                       reports how many survived and which reset. Returns the validation report + content_hash \
                       + (on success) that diff summary; ok:false installs nothing and the old sound keeps \
                       playing. Pass `expect` (a content_hash) to guard against a stale swap — a mismatch \
                       returns a conflict, no install.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<SwapToolOutput>()
            .expect("SwapToolOutput is an object schema")
    )]
    async fn swap(
        &self,
        Parameters(params): Parameters<SwapParams>,
    ) -> Result<CallToolResult, McpError> {
        match self
            .engine
            .structure()
            .swap(DocSource::Path(params.path), params.expect)
        {
            // An install report — success OR ok:false load failure — is an ORDINARY result: the
            // channel worked, the report is the deliverable.
            Ok(SwapOutcome::Installed(report)) => {
                let summary = swap_summary(&report);
                structured_ok(&SwapToolOutput::installed(report), summary)
            }
            // An `expect`-guard miss is the guard guarding, not the tool failing:
            // nothing installed, ordinary result carrying the conflict to reconcile.
            Ok(SwapOutcome::Conflict(conflict)) => {
                let summary = format!(
                    "swap rejected by the expect guard: the engine is playing {}, not the \
                     expected {} — re-read with get_current_instrument and reconcile",
                    conflict.actual, conflict.expected
                );
                structured_ok(&SwapToolOutput::conflict(conflict), summary)
            }
            Err(why) => Ok(map_structure_err("the swap could not be completed", why)),
        }
    }

    /// Read the canonical installed document. Act-then-map: an unreachable engine ⇒
    /// `isError`. Forwards the structure-channel `get_document` and returns `{ document, content_hash }`.
    #[tool(
        name = "get_current_instrument",
        description = "Return the document the engine is currently playing, with its content hash (the token a \
                       later swap's `expect` guard compares). Fails fast if no engine is reachable.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<DocumentSnapshot>()
            .expect("DocumentSnapshot is an object schema")
    )]
    async fn get_current_instrument(&self) -> Result<CallToolResult, McpError> {
        match self.engine.structure().get_document() {
            Ok(snapshot) => {
                let summary = format!(
                    "current instrument (content_hash {})",
                    snapshot.content_hash
                );
                structured_ok(&snapshot, summary)
            }
            Err(why) => Ok(map_structure_err(
                "could not read the current instrument",
                why,
            )),
        }
    }

    /// Read the engine diagnostics counters. Act-then-map: an unreachable engine ⇒
    /// `isError`. Forwards the structure-channel `get_diagnostics` and returns the four counters.
    #[tool(
        name = "get_diagnostics",
        description = "Return the engine's running diagnostics counters since start: output_xruns (events) plus \
                       input_ring underruns/overruns/producer_drops (frames). Fails fast if no engine is reachable.",
        output_schema = rmcp::handler::server::tool::schema_for_output::<DiagnosticsReport>()
            .expect("DiagnosticsReport is an object schema")
    )]
    async fn get_diagnostics(&self) -> Result<CallToolResult, McpError> {
        match self.engine.structure().get_diagnostics() {
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
            Err(why) => Ok(map_structure_err(
                "could not read the engine diagnostics",
                why,
            )),
        }
    }
}

/// The one place a failed structure exchange becomes a tool result, so the three structure-reading
/// tools cannot classify it differently.
///
/// The split is the error-layer discipline: an **unreachable** engine gets the shared fail-fast
/// result carrying the "start `reuben play`" guidance, because that is the one failure the user can
/// act on. Anything else — the engine answered, but with a channel-level fault rather than a domain
/// answer — is the tool genuinely unable to do its job, reported under `context` so the model
/// learns *which* call died.
fn map_structure_err(context: &str, why: StructureError) -> CallToolResult {
    if why.is_unreachable() {
        return engine_unreachable();
    }
    CallToolResult::error(vec![ContentBlock::text(format!("{context}: {why}"))])
}

impl Default for ReubenServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ReubenServer {
    /// Declare the `tools` and `resources` capabilities and the `instructions` field.
    /// Providing `get_info` ourselves is what lets us add `resources` beside the tool
    /// router's `tools`; the `resources` capability is a **static** set — no subscribe/listChanged
    /// — served by [`list_resources`](Self::list_resources) /
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

    /// The static resource set: the authoring guide,
    /// the intent vocabulary, and the library index. No `subscribe`/`listChanged` — the
    /// capability builder declares neither, and this list never changes over a session, so there
    /// is no cursor to page.
    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(ListResourcesResult::with_all_items(
            RESOURCES
                .iter()
                .map(|r| {
                    Resource::new(r.uri, r.name)
                        .with_title(r.title)
                        .with_description(r.description)
                        .with_mime_type(r.mime)
                })
                .collect(),
        ))
    }

    /// Read one static resource, served at request
    /// time: each is read from disk — never `include_str!`, so a sidecar built
    /// yesterday still serves today's content — at its checkout path (env-overridable per
    /// resource, else the compile-time checkout path). An unknown URI is `resource_not_found`.
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri = request.uri.as_str();
        match RESOURCES.iter().find(|r| r.uri == uri) {
            Some(entry) => Ok(ReadResourceResult::new(vec![entry.read_contents()?])),
            None => Err(McpError::resource_not_found(
                // Name what IS served straight off the roster (single-sourced, so a 4th row can't be
                // added while this message keeps listing only the original three), read as an English
                // Oxford list so the prose matches the pre-refactor "X, Y, and Z" bit-for-bit.
                format!(
                    "unknown resource `{uri}`; this server serves {}",
                    served_resource_uris()
                ),
                None,
            )),
        }
    }
}

/// Convert a `send` message's JSON args into the flat primitive [`Arg`]s the OSC encoder packs
/// (args are `number | string`). An integer within `i32` range maps to `Arg::I32`,
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
/// text block. The structured content is what the model acts on; the text is the
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

/// The isError result for a can't-do-the-job document-loading failure: an ambiguous
/// or missing one-of, or an unreadable path. `isError` tells the model to act on the guidance
/// rather than treat the payload as a deliverable.
fn cannot_load(message: impl Into<String>) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(message.into())])
}

/// Resolve a [`DocumentParams`] one-of into the instrument JSON plus a stat-only [`FsResolver`]
/// Exactly one of `path`/`document` is required. `Ok` carries the JSON
/// text and a resolver rooted for nested references; `Err` is the ready-to-return `isError` result
/// for a bad one-of or an unreadable path — the can't-do-the-job cases.
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
            // for the same reason as the path branch.
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

/// Serve the MCP protocol over stdio until the client closes the connection. The
/// current_thread runtime is built by `main`; this is the async body it drives. `main` injects the
/// [`EngineLink`], so the composition root stays in `main`.
pub async fn serve_stdio(engine: EngineLink) -> Result<(), Box<dyn std::error::Error>> {
    let service = ReubenServer::with_engine(engine)
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    use reuben_core::coordinator::{Request, Response, DEFAULT_STRUCTURE_ADDR};
    use std::io;
    use std::net::UdpSocket;
    use std::time::Duration;

    /// A [`StructureTransport`] answering with canned NDJSON instead of dialing a socket — the
    /// seam the engine-tool unit tests inject.
    ///
    /// It substitutes only the *bytes on the wire*, so everything above it runs for real: the tool
    /// body serializes a genuine [`Request`] (parsed here, so a malformed one is a failed test),
    /// [`StructureClient`] parses a genuine [`Response`], and an unconfigured verb returns the same
    /// [`io::Error`] a dead socket would — taking the real path to
    /// [`StructureError::Unreachable`] rather than a hand-written stand-in for it.
    #[derive(Debug, Default)]
    struct FakeTransport {
        ping: Option<Response>,
        swap: Option<Response>,
        document: Option<Response>,
        diagnostics: Option<Response>,
    }

    impl FakeTransport {
        /// A reachable engine — `ping` answers `pong` — with no other verb configured yet.
        fn reachable() -> Self {
            Self {
                ping: Some(Response::Pong),
                ..Self::default()
            }
        }

        /// A down engine: every verb, `ping` included, fails the way a refused connect does.
        fn unreachable() -> Self {
            Self::default()
        }

        fn with_swap(mut self, response: Response) -> Self {
            self.swap = Some(response);
            self
        }

        fn with_document(mut self, snapshot: DocumentSnapshot) -> Self {
            self.document = Some(Response::Document(snapshot));
            self
        }

        fn with_diagnostics(mut self, report: DiagnosticsReport) -> Self {
            self.diagnostics = Some(Response::Diagnostics(report));
            self
        }
    }

    impl StructureTransport for FakeTransport {
        fn round_trip(&self, line: &str, _read_timeout: Duration) -> io::Result<String> {
            let request = Request::from_ndjson(line)
                .expect("the tool body must put a well-formed request line on the wire");
            let configured = match request {
                Request::Ping => &self.ping,
                Request::Swap { .. } => &self.swap,
                Request::GetDocument => &self.document,
                Request::GetDiagnostics => &self.diagnostics,
            };
            match configured {
                Some(response) => Ok(response.to_ndjson()),
                // An unconfigured verb models a down engine, delivered exactly as a dead port
                // does: an io::Error the client classifies, not a pre-classified StructureError.
                None => Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "connection refused",
                )),
            }
        }

        fn endpoint(&self) -> &str {
            DEFAULT_STRUCTURE_ADDR
        }
    }

    /// Drive an async tool body on the current-thread runtime (the only rt feature this crate enables).
    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("current-thread runtime")
            .block_on(future)
    }

    /// A server whose structure channel answers from `transport`, paired with the UDP socket
    /// standing in for the engine's OSC-in endpoint.
    ///
    /// The OSC plane is **not** faked: `send` really binds a socket and really dispatches
    /// datagrams, which really arrive here — so the tests assert on encoded bytes rather than on a
    /// counter, and nothing leaks to a developer's live `reuben play` on the default port.
    fn server_with_osc(transport: FakeTransport) -> (ReubenServer, UdpSocket) {
        let osc = UdpSocket::bind("127.0.0.1:0").expect("bind a stand-in OSC endpoint");
        osc.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("bounded read so a missing datagram fails instead of hanging");
        let osc_addr = osc.local_addr().expect("stand-in OSC address").to_string();
        let server = ReubenServer::with_engine(EngineLink::from_parts(
            StructureClient::with_transport(transport, Duration::from_secs(5)),
            osc_addr,
        ));
        (server, osc)
    }

    /// [`server_with_osc`] for the tools that never touch the OSC plane.
    fn server_with(transport: FakeTransport) -> ReubenServer {
        server_with_osc(transport).0
    }

    /// Assert that a payload a tool really returns is described by the `outputSchema` that tool
    /// really advertises.
    ///
    /// Two derives read the same struct — `Serialize` decides what goes on the wire, `JsonSchema`
    /// decides what we *promise* goes on the wire — and they diverge exactly where attributes are
    /// involved (`#[serde(flatten)]`, `skip_serializing_if`, `rename`). MCP requires
    /// `structuredContent` to conform to the declared schema, so a divergence breaks every
    /// conforming client while every existing test stays green.
    ///
    /// Deliberately NOT a snapshot: nothing here records what the schema *is*. Renaming a type,
    /// re-wording a doc comment, adding a field, or reordering properties all stay green — those
    /// are API changes, not defects. It goes red only when the promise and the payload disagree.
    fn assert_payload_conforms<T: Serialize + schemars::JsonSchema + 'static>(
        what: &str,
        payload: &T,
    ) {
        let schema = serde_json::to_value(
            rmcp::handler::server::tool::schema_for_output::<T>().expect("an object schema"),
        )
        .expect("schema as value");
        let value = serde_json::to_value(payload).expect("payload as value");

        let declared: std::collections::BTreeSet<&str> = schema["properties"]
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let emitted: std::collections::BTreeSet<&str> = value
            .as_object()
            .map(|o| o.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let required: std::collections::BTreeSet<&str> = schema["required"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let undeclared: Vec<_> = emitted.difference(&declared).collect();
        assert!(
            undeclared.is_empty(),
            "{what}: serialized key(s) {undeclared:?} are absent from the advertised \
             outputSchema — a client validating against it would reject a valid response.\n\
             schema properties: {declared:?}\npayload keys: {emitted:?}"
        );

        let promised_but_missing: Vec<_> = required.difference(&emitted).collect();
        assert!(
            promised_but_missing.is_empty(),
            "{what}: outputSchema marks {promised_but_missing:?} required, but this payload \
             omits them — a conforming client would reject a response the tool really sends.\n\
             required: {required:?}\npayload keys: {emitted:?}"
        );
    }

    #[test]
    fn engine_tool_payloads_conform_to_their_advertised_output_schemas() {
        // Every engine tool that returns a structured payload, in every shape it can return one.
        // The two SwapToolOutput branches are checked separately because `flatten` +
        // `skip_serializing_if` make them structurally different objects.
        assert_payload_conforms(
            "swap (clean install)",
            &SwapToolOutput::installed(SwapReport {
                report: Report {
                    ok: true,
                    errors: vec![],
                    warnings: vec![],
                },
                content_hash: "00c0ffee".to_string(),
                diff: Some(reuben_core::DiffSummary::default()),
            }),
        );
        assert_payload_conforms(
            "swap (validation failure)",
            &SwapToolOutput::installed(SwapReport::rejected("00c0ffee".to_string())),
        );
        assert_payload_conforms(
            "swap (expect-guard miss)",
            &SwapToolOutput::conflict(Conflict {
                expected: "0badc0de".to_string(),
                actual: "00c0ffee".to_string(),
            }),
        );
        assert_payload_conforms(
            "get_current_instrument",
            &DocumentSnapshot {
                document: serde_json::json!({ "instrument": "t" }),
                content_hash: "00c0ffee".to_string(),
            },
        );
        assert_payload_conforms("get_diagnostics", &DiagnosticsReport::default());
        assert_payload_conforms(
            "engine_status",
            &EngineStatusOutput {
                reachable: false,
                endpoints: StatusEndpoints {
                    structure: DEFAULT_STRUCTURE_ADDR.to_string(),
                    osc: default_osc_addr(),
                },
                sidecar: SidecarInfo {
                    version: env!("CARGO_PKG_VERSION").to_string(),
                    format_version: reuben_core::format::FORMAT_VERSION,
                },
                guidance: Some(ENGINE_UNREACHABLE_GUIDANCE.to_string()),
            },
        );
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
    fn resolve_checkout_path_prefers_the_override_then_the_default() {
        // #496 (folding #374 + R9 #466): the pure override-vs-default logic, tested by calling
        // `resolve_checkout_path` directly with explicit args — no process-env mutation, so it can't
        // flake and it keeps a genuine direct caller for the function's "unit-testable without racing
        // on real env vars" rationale honest. A present override wins for a non-checkout deploy; an
        // absent one falls back to the compile-time checkout default.
        assert_eq!(
            resolve_checkout_path(
                Some(std::ffi::OsString::from("/opt/reuben/override.md")),
                "/checkout/default.md",
            ),
            std::path::PathBuf::from("/opt/reuben/override.md"),
            "a present override wins"
        );
        assert_eq!(
            resolve_checkout_path(None, "/checkout/default.md"),
            std::path::PathBuf::from("/checkout/default.md"),
            "an absent override falls back to the default"
        );
    }

    #[test]
    fn every_resource_env_field_drives_its_resolve_path() {
        // #496: for every row, prove the row's OWN `entry.env` field is what `entry.resolve_path()`
        // reads — a `resolve_path` that read a hardcoded var instead of `self.env`, or a row with a
        // cross-wired `env`, fails here. Driven through the production method (not the pure fn).
        //
        // `set_var`/`remove_var` are process-global and cargo runs tests as threads in one process,
        // so this is only safe because these three REUBEN_* resource vars are read by NO other inline
        // test. Per row the env is mutated panic-safely: set, capture BOTH outcomes into locals, and
        // `remove_var` BEFORE any assertion runs — so a failed assert can never leak a var into a
        // sibling test.
        for (i, entry) in RESOURCES.iter().enumerate() {
            // Fixture path independent of any entry field — the override just has to differ from the
            // default and round-trip through `resolve_path` unchanged.
            let override_path = format!("/opt/reuben/resource-{i}.md");

            std::env::set_var(entry.env, &override_path);
            let got_override = entry.resolve_path();
            std::env::remove_var(entry.env);
            let got_default = entry.resolve_path();

            // Only now, with the env already cleaned, do the assertions.
            assert_eq!(
                got_override,
                std::path::PathBuf::from(&override_path),
                "the row's own `env` ({}) must select the override for {}",
                entry.env,
                entry.uri
            );
            assert_eq!(
                got_default,
                std::path::PathBuf::from(entry.default_path),
                "unset falls back to the compile-time checkout default: {}",
                entry.uri
            );
        }
    }

    #[test]
    fn resource_table_is_self_consistent() {
        // #496 the "new resource = one row" guard: no duplicate URIs, env overrides, or default
        // paths (a copy-pasted row that forgot to update any of them would silently alias another
        // resource's wire URI, REUBEN_* override, or served file), and every MIME is text/markdown
        // (all three resources are CommonMark prose).
        use std::collections::HashSet;
        let mut uris = HashSet::new();
        let mut envs = HashSet::new();
        let mut paths = HashSet::new();
        for entry in RESOURCES {
            assert!(
                uris.insert(entry.uri),
                "RESOURCES must not contain a duplicate URI: {}",
                entry.uri
            );
            assert!(
                envs.insert(entry.env),
                "RESOURCES must not reuse an env override: {}",
                entry.env
            );
            assert!(
                paths.insert(entry.default_path),
                "RESOURCES must not reuse a default path: {}",
                entry.default_path
            );
            assert_eq!(
                entry.mime, "text/markdown",
                "every resource is CommonMark prose: {}",
                entry.uri
            );
        }
    }

    #[test]
    fn served_resource_uris_reads_as_an_oxford_list() {
        // #496: the unknown-resource guidance names every served URI. The list is single-sourced
        // from RESOURCES, but the assembled prose is unguarded by stdio_resources.rs (which only
        // does substring `.contains()` checks), so pin the exact 3-resource wording bit-for-bit —
        // an Oxford comma before the final `and`, matching the pre-refactor message.
        assert_eq!(
            served_resource_uris(),
            format!(
                "{GUIDE_RESOURCE_URI}, {VOCABULARY_RESOURCE_URI}, and {LIBRARY_INDEX_RESOURCE_URI}"
            ),
            "the served-URI list must read as an Oxford list for the current 3-resource roster"
        );
    }

    #[test]
    fn swap_result_serializes_report_hash_and_diff() {
        // A successful swap serializes as the shared SwapReport shape —
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
        // engine_status answers "reachable?", so it is NEVER isError — even on a dead
        // engine it reports the down state (reachable:false + guidance) as its deliverable, with the
        // endpoints and sidecar identity still filled in.
        let result = block_on(server_with(FakeTransport::unreachable()).engine_status())
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
        let result = block_on(server_with(FakeTransport::reachable()).engine_status())
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
        // (a) The advertised input schema declares minItems:1.
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
            server_with(FakeTransport::reachable())
                .send(Parameters(SendParams { messages: vec![] })),
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
        // A document read against a down engine is a can't-do-the-job isError carrying
        // the "start `reuben play`" guidance (act-then-map on get_document).
        let result = block_on(server_with(FakeTransport::unreachable()).get_current_instrument())
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
        // An expect-guard miss is an ORDINARY result (the guard
        // guarding, not the tool failing), NOT isError; nothing is installed (ok:false, no diff),
        // and the conflict names both hashes field-for-field so the model reconciles.
        let fake = FakeTransport::reachable().with_swap(Response::Conflict(Conflict {
            expected: "0badc0de".to_string(),
            actual: "00c0ffee".to_string(),
        }));
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
    fn swap_relays_diff_summary_verbatim() {
        // The tool relays the channel's diff unchanged. Fixture: an all-cold swap
        // (nothing survived), which is what the web lane always reports and what a native swap
        // reports when no node survives.
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
        let fake = FakeTransport::reachable().with_swap(Response::SwapReport(report));
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
            "the channel's survived count is relayed verbatim: {s}"
        );
    }

    #[test]
    fn swap_unreachable_is_iserror() {
        // Act-then-map on the mutating verb: a down engine is the fail-fast isError.
        let result = block_on(server_with(FakeTransport::unreachable()).swap(Parameters(
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
        // A reachable send encodes every message and reports the count dispatched — and the
        // datagrams really leave a socket: this asserts on what ARRIVES at a stand-in OSC endpoint,
        // so the encoding is proven end-to-end rather than counted through a fake.
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
        let (server, osc) = server_with_osc(FakeTransport::reachable());
        let result = block_on(server.send(Parameters(params))).expect("result");
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

        // Both datagrams arrived, each carrying its OSC address as the leading string.
        let mut received = Vec::new();
        for _ in 0..2 {
            let mut buf = [0u8; 1024];
            let (n, _) = osc
                .recv_from(&mut buf)
                .expect("a dispatched datagram arrives");
            received.push(buf[..n].to_vec());
        }
        let addresses: Vec<String> = received
            .iter()
            .map(|d| {
                let end = d.iter().position(|&b| b == 0).unwrap_or(d.len());
                String::from_utf8_lossy(&d[..end]).into_owned()
            })
            .collect();
        assert!(
            addresses.contains(&"/voice1/cutoff".to_string())
                && addresses.contains(&"/voice1/notes".to_string()),
            "both OSC addresses must arrive on the wire: {addresses:?}"
        );
    }

    #[test]
    fn send_unreachable_is_iserror() {
        // Probe-first: datagrams encode fine, but a down engine fails the ping.
        let params = SendParams {
            messages: vec![OscSendMessage {
                address: "/voice1/cutoff".to_string(),
                args: vec![serde_json::json!(1.0)],
            }],
        };
        let result = block_on(server_with(FakeTransport::unreachable()).send(Parameters(params)))
            .expect("result");
        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("reuben play"));
    }

    #[test]
    fn send_rejects_a_non_scalar_argument() {
        // Args are number | string; a nested array is a can't-do-the-job error,
        // caught before any dispatch (and without needing the engine).
        let params = SendParams {
            messages: vec![OscSendMessage {
                address: "/voice1/cutoff".to_string(),
                args: vec![serde_json::json!([1, 2, 3])],
            }],
        };
        let result = block_on(server_with(FakeTransport::reachable()).send(Parameters(params)))
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
            server_with(FakeTransport::reachable().with_document(snapshot))
                .get_current_instrument(),
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
            server_with(FakeTransport::reachable().with_diagnostics(report)).get_diagnostics(),
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
            block_on(server_with(FakeTransport::unreachable()).get_diagnostics()).expect("result");
        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("reuben play"));
    }

    #[test]
    fn the_declared_roster_is_registered() {
        // The router advertises exactly the declared roster (derived from
        // reuben_core::tools::CONTRACTS via tool_names) — the same surface the stdio integration
        // test asserts over the wire, checked here without spawning a process.
        let server = ReubenServer::new();
        let mut advertised: Vec<String> = server
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        advertised.sort();

        let mut expected: Vec<String> = tool_names().iter().map(|n| n.to_string()).collect();
        expected.sort();

        assert_eq!(
            advertised, expected,
            "the tool surface must be the declared roster"
        );
    }
}
