//! reuben-mcp — the per-conversation MCP stdio sidecar.
//!
//! A [`ServerHandler`] with a tool router over the [`reuben_api::tools::CONTRACTS`] roster, in
//! three families: the pure introspection tools and the document verbs answer in-process through
//! [`reuben_api::authoring`], over a [`reuben_api::FsResolver`] filling its resource seam, while
//! the engine tools reach a user-owned `reuben play` through [`reuben_api::engine`], over the
//! loopback transport in [`EngineLink`].
//!
//! What is left here is the MCP-shaped part and only that: the roster, the transport, and the
//! two-way map between a window answer and a `CallToolResult`. The argument shapes, the result
//! shapes, the guards and the glosses belong to the window, so a second door inherits them rather
//! than reimplementing them.
//!
//! That includes the per-tool `description` sentences, which the window owns too. rmcp's `#[tool]`
//! does take a string **literal** there and cannot name a const, so the attribute is left off and
//! [`stamp_window_prose`] writes the window's sentence onto the built router instead — the same
//! prose the CLI and the browser read, rather than a copy per door.
//!
//! The `name` argument is literal-only for the same reason, so the roster spelling is written out
//! once per tool here — redundantly, since rmcp defaults a route's name to the method's own ident
//! and every method is already named for its verb, but written anyway so the wire name is read off
//! the attribute rather than inferred from a default. Nothing about that literal is coupled to the
//! roster, and [`stamp_window_prose`]'s two assertions are what stand in for the coupling: at
//! construction, in both directions, refusing to start rather than serve a surface that is not the
//! roster.
//!
//! Everywhere the door names a contract as a *value* rather than as a route key it writes
//! [`reuben_api::tools::names`] instead, so the window dropping that contract is a compile error
//! here. Today that is all test code, because the route key is the only place non-test source
//! names a verb at all: the door reaches every one of them by calling the window function that
//! serves it, and a function is not a string.
//!
//! The one thing left that is genuinely this door's is the socket: `reuben-mcp` reaches a
//! *separate process*, so it supplies a loopback TCP transport where an in-process host supplies
//! none at all.
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

use reuben_api::authoring::{self, Answer, EditResult, Refusal};
use reuben_api::engine;
use reuben_api::FsResolver;
use serde::Serialize;

mod engine_link;
pub use engine_link::{EngineLink, TcpTransport, DEFAULT_CONNECT_TIMEOUT};

/// The window's engine-half surface, re-exported for a test or a caller driving this door: the
/// channel a fake transport plugs into, and the payloads it carries.
pub use reuben_api::engine::{
    Channel, ChannelError, Conflict, DocumentSnapshot, SwapOutcome, Transport,
    ENGINE_UNREACHABLE_GUIDANCE,
};

/// The tool surface this door advertises, in roster order — the exact spellings on `tools/list`,
/// so the wire surface can only change by changing the roster. see rules: agent-mcp
pub fn tool_names() -> Vec<&'static str> {
    reuben_api::tools::names()
}

/// The `reuben://guide/authoring` resource URI: the authoring guide, `docs/agents/authoring.md`.
/// The authority for what `resources/list` advertises.
pub const GUIDE_RESOURCE_URI: &str = "reuben://guide/authoring";

/// The MIME type advertised for [`GUIDE_RESOURCE_URI`]: the authoring guide is CommonMark prose.
pub const GUIDE_RESOURCE_MIME: &str = "text/markdown";

/// The `reuben://guide/vocabulary` resource URI: the rendered intent→parameter vocabulary,
/// `docs/agents/vocabulary.md` — generated, and staleness-tested against the registry elsewhere.
pub const VOCABULARY_RESOURCE_URI: &str = "reuben://guide/vocabulary";

/// The MIME type advertised for [`VOCABULARY_RESOURCE_URI`]: the rendered vocabulary is
/// CommonMark prose.
pub const VOCABULARY_RESOURCE_MIME: &str = "text/markdown";

/// The library-index resource URI: the generated signature-line index over the available
/// instrument set, `instruments/index.md`. Shares the `guide/` namespace with the other two —
/// one namespace for all agent-read grounding.
pub const LIBRARY_INDEX_RESOURCE_URI: &str = "reuben://guide/library-index";

/// The MIME type advertised for [`LIBRARY_INDEX_RESOURCE_URI`]: the generated index is CommonMark
/// prose.
pub const LIBRARY_INDEX_RESOURCE_MIME: &str = "text/markdown";

/// The server `instructions`: the one-breath authoring gist, pointing at the three guide
/// resources rather than restating them. see rules: agent-mcp
const INSTRUCTIONS: &str = "reuben authoring sidecar. The instrument document is the durable \
     truth; keep it in sync with the sound. **Never open, read or write an instrument file \
     yourself** — name it by `source` and let these tools do it: `describe_instrument` reads its \
     structure, the `*_instrument_*` verbs edit it one change at a time, and each one re-validates \
     the whole document and writes only if it is valid. Start `reuben play` in another terminal \
     first — the engine tools (`send_live_controls`, `swap_instrument`, `get_current_instrument`, \
     `get_engine_diagnostics`) fail fast until it is reachable. The loop: `send_live_controls` to \
     audition a change (ephemeral — clobbered at the next swap), then edit the document and \
     `swap_instrument` to make it durable. Creating an instrument from scratch? Call \
     `new_instrument` to land a guaranteed-valid seed at a source, then add nodes and wire them. \
     Read `reuben://guide/authoring` \
     for the type system, wiring rules, instrument format, and the authoring loop. Read \
     `reuben://guide/vocabulary` for the word→move table translating intent language (\"warmer\", \
     \"busier\", \"sadder\") into parameter moves. Read `reuben://guide/library-index` for the \
     available instruments to reuse by reference through a `subpatch` node.";

/// Default absolute path to the authoring guide (`docs/agents/authoring.md`). Only the *path* is
/// compile-time — the file itself is read at request time — and it is valid only in the checkout
/// the sidecar was built in; a deploy outside one overrides it with [`AUTHORING_GUIDE_ENV`].
///
/// see rules: agent-mcp
const AUTHORING_GUIDE_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/agents/authoring.md"
);

/// Default absolute path to the rendered vocabulary (`docs/agents/vocabulary.md`) — the same
/// posture as [`AUTHORING_GUIDE_PATH`], overridden by [`VOCABULARY_ENV`].
const VOCABULARY_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../docs/agents/vocabulary.md"
);

/// Default absolute path to the generated library index (`instruments/index.md`) — the same
/// posture as [`AUTHORING_GUIDE_PATH`], overridden by [`LIBRARY_INDEX_ENV`].
const LIBRARY_INDEX_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../instruments/index.md");

/// Env override for the authoring-guide path, for a **non-checkout deploy**: a shipped sidecar
/// binary whose [`AUTHORING_GUIDE_PATH`] points into a build checkout that need not exist where it
/// runs. Unset keeps the compile-time default.
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
    /// A server backed by an [`EngineLink`] on the shared default endpoint
    /// (the window's `DEFAULT_STRUCTURE_ADDR`): the engine tools reach a live
    /// `reuben play` over the real structure channel. The binary's composition root (`main`)
    /// injects the link via [`with_engine`](Self::with_engine); this is the sensible default.
    pub fn new() -> Self {
        Self::with_engine(EngineLink::default())
    }

    /// A server with an explicit engine link — the injection point for tests, which pair it with
    /// a fake structure transport.
    pub fn with_engine(engine: EngineLink) -> Self {
        let mut tool_router = Self::tool_router();
        stamp_window_prose(&mut tool_router);
        Self {
            tool_router,
            engine,
        }
    }

    // --- Pure tools: always available -----------------------------------------------------------

    /// List the operator set. Engine-free — always available, and the one authoring tool that needs
    /// no resource store at all.
    #[tool(
        name = "describe_operators",
        output_schema = rmcp::handler::server::tool::schema_for_output::<authoring::Operators>()
            .expect("Operators is an object schema")
    )]
    async fn describe_operators(
        &self,
        Parameters(p): Parameters<authoring::DescribeOperators>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::describe_operators(&p))
    }

    /// Read a structural view of an instrument document.
    #[tool(
        name = "describe_instrument",
        output_schema = rmcp::handler::server::tool::schema_for_output::<authoring::DocumentView>()
            .expect("DocumentView is an object schema")
    )]
    async fn describe_instrument(
        &self,
        Parameters(p): Parameters<authoring::DescribeInstrument>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::describe_instrument(&p, &store(&p.source)))
    }

    /// Validate an instrument document through the engine's own load + instantiate path.
    #[tool(
        name = "validate_instrument",
        output_schema = rmcp::handler::server::tool::schema_for_output::<authoring::Report>()
            .expect("Report is an object schema")
    )]
    async fn validate_instrument(
        &self,
        Parameters(p): Parameters<authoring::ValidateInstrument>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::validate_instrument(&p, &store(&p.source)))
    }

    // --- Engine tools: reach a user-owned `reuben play` through the channel seam ----------------

    // Each is one delegation, like the document verbs above: the batch bounds, the argument
    // conversion, the unreachable/other split, the guard-miss shape and the glosses all live behind
    // `engine`. What this door still owns is the socket under the channel, the advertised schema,
    // and the isError decision. see rules: agent-mcp

    #[tool(
        name = "send_live_controls",
        output_schema = rmcp::handler::server::tool::schema_for_output::<engine::SendOutput>()
            .expect("SendOutput is an object schema")
    )]
    async fn send_live_controls(
        &self,
        Parameters(p): Parameters<engine::SendLiveControls>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::send_live_controls(&p, self.engine.structure()))
    }

    /// The one engine tool that is **never** `isError` for a dead engine — answering "reachable?"
    /// is its job, so the window hands back an answer and never a refusal.
    #[tool(
        name = "get_engine_status",
        output_schema = rmcp::handler::server::tool::schema_for_output::<engine::EngineStatus>()
            .expect("EngineStatus is an object schema")
    )]
    async fn get_engine_status(&self) -> Result<CallToolResult, McpError> {
        // The door names itself: its own version is the one identity the window cannot know.
        let answer = engine::get_engine_status(self.engine.structure(), env!("CARGO_PKG_VERSION"));
        structured_ok(&answer.output, answer.summary)
    }

    #[tool(
        name = "swap_instrument",
        output_schema = rmcp::handler::server::tool::schema_for_output::<engine::SwapResult>()
            .expect("SwapResult is an object schema")
    )]
    async fn swap_instrument(
        &self,
        Parameters(p): Parameters<engine::SwapInstrument>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::swap_instrument(&p, self.engine.structure()))
    }

    #[tool(
        name = "get_current_instrument",
        output_schema =
            rmcp::handler::server::tool::schema_for_output::<engine::CurrentInstrument>()
                .expect("CurrentInstrument is an object schema")
    )]
    async fn get_current_instrument(&self) -> Result<CallToolResult, McpError> {
        answered(engine::get_current_instrument(
            self.engine.structure(),
            installed_store,
        ))
    }

    #[tool(
        name = "get_engine_diagnostics",
        output_schema =
            rmcp::handler::server::tool::schema_for_output::<engine::DiagnosticsReport>()
                .expect("DiagnosticsReport is an object schema")
    )]
    async fn get_engine_diagnostics(&self) -> Result<CallToolResult, McpError> {
        answered(engine::get_engine_diagnostics(self.engine.structure()))
    }

    // --- Document tools: engine-free mutators over an instrument document -------------------------
    //
    // One roster entry per window verb, and nothing else. There is deliberately nothing else here:
    // the `expect` guard, the write-iff-valid pipeline, the projection echo, the one-line gloss and
    // the advertised sentence all live behind `authoring`, so a second door gets them without a
    // second copy. What the door still owns is the roster spelling, the advertised schema, and the
    // isError decision. see rules: agent-mcp

    #[tool(
        name = "new_instrument",
        output_schema = edit_result_schema()
    )]
    async fn new_instrument(
        &self,
        Parameters(p): Parameters<authoring::NewInstrument>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::new_instrument(&p, &store(&p.source)))
    }

    #[tool(
        name = "set_instrument_name",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_name(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentName>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::set_instrument_name(&p, &store(&p.source)))
    }

    #[tool(
        name = "set_instrument_description",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_description(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentDescription>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::set_instrument_description(&p, &store(&p.source)))
    }

    #[tool(
        name = "add_instrument_node",
        output_schema = edit_result_schema()
    )]
    async fn add_instrument_node(
        &self,
        Parameters(p): Parameters<authoring::AddInstrumentNode>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::add_instrument_node(&p, &store(&p.source)))
    }

    #[tool(
        name = "remove_instrument_node",
        output_schema = edit_result_schema()
    )]
    async fn remove_instrument_node(
        &self,
        Parameters(p): Parameters<authoring::RemoveInstrumentNode>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::remove_instrument_node(&p, &store(&p.source)))
    }

    #[tool(
        name = "rename_instrument_node",
        output_schema = edit_result_schema()
    )]
    async fn rename_instrument_node(
        &self,
        Parameters(p): Parameters<authoring::RenameInstrumentNode>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::rename_instrument_node(&p, &store(&p.source)))
    }

    #[tool(
        name = "set_instrument_node_description",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_node_description(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentNodeDescription>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::set_instrument_node_description(
            &p,
            &store(&p.source),
        ))
    }

    #[tool(
        name = "set_instrument_input",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_input(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentInput>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::set_instrument_input(&p, &store(&p.source)))
    }

    #[tool(
        name = "set_instrument_inputs_by_intent",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_inputs_by_intent(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentInputsByIntent>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::set_instrument_inputs_by_intent(
            &p,
            &store(&p.source),
        ))
    }

    #[tool(
        name = "wire_instrument_input",
        output_schema = edit_result_schema()
    )]
    async fn wire_instrument_input(
        &self,
        Parameters(p): Parameters<authoring::WireInstrumentInput>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::wire_instrument_input(&p, &store(&p.source)))
    }

    #[tool(
        name = "unwire_instrument_input",
        output_schema = edit_result_schema()
    )]
    async fn unwire_instrument_input(
        &self,
        Parameters(p): Parameters<authoring::UnwireInstrumentInput>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::unwire_instrument_input(&p, &store(&p.source)))
    }

    #[tool(
        name = "set_instrument_constant",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_constant(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentConstant>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::set_instrument_constant(&p, &store(&p.source)))
    }

    #[tool(
        name = "add_instrument_interface_input",
        output_schema = edit_result_schema()
    )]
    async fn add_instrument_interface_input(
        &self,
        Parameters(p): Parameters<authoring::AddInstrumentInterfaceInput>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::add_instrument_interface_input(
            &p,
            &store(&p.source),
        ))
    }

    #[tool(
        name = "add_instrument_interface_output",
        output_schema = edit_result_schema()
    )]
    async fn add_instrument_interface_output(
        &self,
        Parameters(p): Parameters<authoring::AddInstrumentInterfaceOutput>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::add_instrument_interface_output(
            &p,
            &store(&p.source),
        ))
    }

    #[tool(
        name = "remove_instrument_interface_input",
        output_schema = edit_result_schema()
    )]
    async fn remove_instrument_interface_input(
        &self,
        Parameters(p): Parameters<authoring::RemoveInstrumentInterfacePipe>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::remove_instrument_interface_input(
            &p,
            &store(&p.source),
        ))
    }

    #[tool(
        name = "remove_instrument_interface_output",
        output_schema = edit_result_schema()
    )]
    async fn remove_instrument_interface_output(
        &self,
        Parameters(p): Parameters<authoring::RemoveInstrumentInterfacePipe>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::remove_instrument_interface_output(
            &p,
            &store(&p.source),
        ))
    }

    #[tool(
        name = "set_instrument_interface_input_meta",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_interface_input_meta(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentInterfaceInputMeta>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::set_instrument_interface_input_meta(
            &p,
            &store(&p.source),
        ))
    }

    #[tool(
        name = "set_instrument_interface_output_meta",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_interface_output_meta(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentInterfaceOutputMeta>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::set_instrument_interface_output_meta(
            &p,
            &store(&p.source),
        ))
    }

    #[tool(
        name = "add_instrument_resource",
        output_schema = edit_result_schema()
    )]
    async fn add_instrument_resource(
        &self,
        Parameters(p): Parameters<authoring::AddInstrumentResource>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::add_instrument_resource(&p, &store(&p.source)))
    }

    #[tool(
        name = "remove_instrument_resource",
        output_schema = edit_result_schema()
    )]
    async fn remove_instrument_resource(
        &self,
        Parameters(p): Parameters<authoring::RemoveInstrumentResource>,
    ) -> Result<CallToolResult, McpError> {
        answered(authoring::remove_instrument_resource(&p, &store(&p.source)))
    }
}

/// Write the window's sentence onto every tool the router carries.
///
/// The sentences belong to the window with the argument and result types they describe, but rmcp's
/// `#[tool]` takes a string **literal** for `description` and so cannot name a const. Stamping the
/// built router is the way to hold both: the attribute is left off entirely, and what the door
/// advertises is decided in one place for every door. Without this the macro falls back to each
/// method's rustdoc, which is written for a Rust reader — so `advertises_the_window_prose` asserts
/// the stamp actually landed rather than trusting it.
///
/// see rules: agent-mcp
fn stamp_window_prose(router: &mut ToolRouter<ReubenServer>) {
    // These two assertions are also what makes the advertised surface *be* the roster, in both
    // directions and at construction: a route the roster does not name keeps rmcp's rustdoc
    // fallback and hands Rust-reader prose to a model, and a roster verb no route serves is
    // advertised by nobody. Refuse to start rather than let either reach the wire — a test that
    // read the wire back could only report it afterwards, and a schema or prose test iterating one
    // of the two lists would stay green through it.
    let unstamped: Vec<String> = router
        .map
        .keys()
        .filter(|name| {
            !reuben_api::tools::CONTRACTS
                .iter()
                .any(|contract| contract.name == name.as_ref())
        })
        .map(|name| name.to_string())
        .collect();
    assert!(
        unstamped.is_empty(),
        "these tools are not on the window's roster and would advertise their rustdoc: \
         {unstamped:?}"
    );

    for contract in reuben_api::tools::CONTRACTS {
        let route = router.map.get_mut(contract.name).unwrap_or_else(|| {
            panic!(
                "the window serves `{}`, but no tool advertises it",
                contract.name
            )
        });
        route.attr.description = Some(contract.description.into());
    }
}

impl Default for ReubenServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for ReubenServer {
    /// Declare the `tools` and `resources` capabilities and the `instructions` field. Providing
    /// `get_info` ourselves is what lets `resources` sit beside the tool router's `tools`.
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

    /// The static resource set. No `subscribe`/`listChanged`, and no cursor to page — the list
    /// never changes over a session.
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

    /// Read one static resource from disk at request time, at its env-overridable checkout path.
    /// An unknown URI is `resource_not_found`. see rules: agent-mcp
    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        let uri = request.uri.as_str();
        match RESOURCES.iter().find(|r| r.uri == uri) {
            Some(entry) => Ok(ReadResourceResult::new(vec![entry.read_contents()?])),
            None => Err(McpError::resource_not_found(
                // Named straight off the roster, so a 4th row cannot be added while this message
                // keeps listing three.
                format!(
                    "unknown resource `{uri}`; this server serves {}",
                    served_resource_uris()
                ),
                None,
            )),
        }
    }
}

/// Build an ordinary (non-error) result carrying BOTH a structured payload — what the model acts on
/// — and a one-line text gloss for a human reading the transcript. A serialization failure is an
/// internal fault, so it surfaces as a protocol error, not as an `isError` deliverable.
fn structured_ok<T: Serialize>(value: &T, summary: String) -> Result<CallToolResult, McpError> {
    let structured = serde_json::to_value(value).map_err(|e| {
        McpError::internal_error(format!("failed to serialize tool output: {e}"), None)
    })?;
    let mut result = CallToolResult::structured(structured);
    // `structured` seeds `content` with a raw JSON dump; replace it with the human summary.
    result.content = vec![ContentBlock::text(summary)];
    Ok(result)
}

/// The one place a window answer becomes a tool result, so no two tools classify one differently.
///
/// An [`Answer`] is ordinary: the structured payload is what the model acts on, the window's gloss
/// is what a human reading the transcript sees. A [`Refusal`] is `isError` — the call could not do
/// its job, so the model must act on the message rather than treat it as a deliverable. Note what
/// is *not* here: a rejected edit and a failing validation are Answers, because the tool worked.
/// see rules: agent-mcp
fn answered<T: Serialize>(answer: Result<Answer<T>, Refusal>) -> Result<CallToolResult, McpError> {
    match answer {
        Ok(answer) => structured_ok(&answer.output, answer.summary),
        Err(refusal) => Ok(CallToolResult::error(vec![ContentBlock::text(
            refusal.message,
        )])),
    }
}

/// The one `outputSchema` every document verb advertises. Derived here rather than at each
/// `#[tool]`, so the roster cannot grow a verb promising a different shape. The attribute itself
/// still repeats per tool — rmcp's macro needs it there — but what it names is decided once.
fn edit_result_schema() -> std::sync::Arc<rmcp::model::JsonObject> {
    rmcp::handler::server::tool::schema_for_output::<EditResult>()
        .expect("EditResult is an object schema")
}

/// The FS door's reading of an opaque document `source`: a filesystem path. Stat-only because no
/// authoring path renders — introspection reports port metadata without decoding referenced audio.
/// see rules: agent-mcp
fn store(source: &str) -> FsResolver {
    FsResolver::for_document(source).stat_only()
}

/// The FS door's store for projecting what the engine handed back, rooted at the installed
/// `source`'s directory when the engine named one and at the sidecar cwd when it did not (an
/// install by value has no directory to anchor against). Best-effort: an unresolvable nested
/// reference degrades the projection rather than failing the call.
fn installed_store(source: Option<&str>) -> FsResolver {
    match source {
        Some(source) => FsResolver::for_instrument(Path::new(source)).stat_only(),
        None => FsResolver::new("."),
    }
    .stat_only()
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

    use reuben_api::authoring::{Diag, Report};
    use reuben_api::engine::{
        ControlArg, ControlMessage, DiagnosticsReport, DiffSummary, Request, Response, SwapReport,
        DEFAULT_STRUCTURE_ADDR,
    };
    use reuben_api::tools::names;
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A [`Transport`] answering with canned NDJSON instead of dialing a socket.
    ///
    /// It substitutes only the *bytes on the wire*: the request line is parsed here, so a malformed
    /// one fails the test, and an unconfigured verb returns the same [`io::Error`] a dead socket
    /// would. see rules: agent-mcp
    #[derive(Debug, Default)]
    struct FakeTransport {
        ping: Option<Response>,
        swap: Option<Response>,
        document: Option<Response>,
        diagnostics: Option<Response>,
        /// Whether the engine serves `send`. Not an `Option<Response>` like the others because the
        /// ack is derived from the batch that arrives, not canned.
        serves_send: bool,
        /// Every [`ControlMessage`] that crossed the wire, in order. Shared, so a test can read it
        /// after the transport has been moved into the client.
        sent: Arc<Mutex<Vec<ControlMessage>>>,
    }

    impl FakeTransport {
        /// A reachable engine — `ping` answers `pong`, `send` acks — with no other verb configured
        /// yet.
        fn reachable() -> Self {
            Self {
                ping: Some(Response::Pong),
                serves_send: true,
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

        /// A reachable engine that acks whatever batch it is handed, plus the shared log of what
        /// crossed the wire — so a `send` test asserts on the *serialized* batch, not on the tool's
        /// own inputs.
        fn recording_send() -> (Self, Arc<Mutex<Vec<ControlMessage>>>) {
            let sent = Arc::new(Mutex::new(Vec::new()));
            let transport = Self {
                sent: Arc::clone(&sent),
                ..Self::reachable()
            };
            (transport, sent)
        }
    }

    impl Transport for FakeTransport {
        fn round_trip(&self, line: &str, _read_timeout: Duration) -> io::Result<String> {
            let request = Request::from_ndjson(line)
                .expect("the tool body must put a well-formed request line on the wire");
            // Answered from the batch that actually arrived, parsed back off the wire — so the ack
            // can only be right if the serialization was.
            if let Request::Send { messages } = request {
                if self.serves_send {
                    self.sent.lock().expect("sent log").extend(messages);
                    return Ok(Response::Sent.to_ndjson());
                }
                return Err(io::Error::new(
                    io::ErrorKind::ConnectionRefused,
                    "connection refused",
                ));
            }
            let configured = match request {
                Request::Ping => &self.ping,
                Request::Swap { .. } => &self.swap,
                Request::GetDocument => &self.document,
                Request::GetDiagnostics => &self.diagnostics,
                Request::Send { .. } => unreachable!("handled above"),
            };
            match configured {
                Some(response) => Ok(response.to_ndjson()),
                // An unconfigured verb models a down engine the way a dead port does: an io::Error
                // for the client to classify, not a pre-classified StructureError.
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

    /// A server whose one engine channel answers from `transport` — the single seam every engine
    /// tool runs through, so there is no second plane to stand in for and no socket to bind.
    fn server_with(transport: FakeTransport) -> ReubenServer {
        ReubenServer::with_engine(EngineLink::from_channel(Channel::with_read_timeout(
            transport,
            Duration::from_secs(5),
        )))
    }

    /// The `outputSchema` a tool actually advertises, pulled off the live router by tool name.
    ///
    /// Read from the router rather than re-deriving it from a type the test names, so the test is
    /// bound to what the tool *declares*. Re-point a `#[tool(output_schema = …)]` at the wrong type
    /// and this notices; re-deriving from the type the test picked would not.
    fn advertised_output_schema(tool: &str) -> serde_json::Value {
        let server = ReubenServer::new();
        let declared = server
            .tool_router
            .list_all()
            .into_iter()
            .find(|t| t.name == tool)
            .unwrap_or_else(|| panic!("`{tool}` is registered"));
        serde_json::to_value(
            declared
                .output_schema
                .as_ref()
                .unwrap_or_else(|| panic!("`{tool}` advertises an outputSchema")),
        )
        .expect("schema as value")
    }

    /// Walk a payload against a schema node, following `$ref` into `$defs` and descending into
    /// nested objects and arrays. Shared sub-schemas (`Conflict`, `Diag`, `DiffSummary`,
    /// `StatusEndpoints`) are checked at their own level, not just at the top.
    fn check_conforms(node: &serde_json::Value, value: &serde_json::Value, ctx: &ConformCtx<'_>) {
        let unhandled = |what: &str| -> ! {
            panic!(
                "{} / {}{}: the conformance walker does not implement {what}, so it would skip \
                 this node and everything beneath it — silently reporting success while checking \
                 nothing. Teach it this shape rather than dropping the case.\nschema node: {}",
                ctx.tool,
                ctx.case,
                ctx.path,
                serde_json::to_string(node).unwrap_or_default()
            )
        };

        // Resolve `$ref: "#/$defs/Name"` before anything else.
        if let Some(reference) = node.get("$ref").and_then(|r| r.as_str()) {
            let Some(name) = reference.strip_prefix("#/$defs/") else {
                // Includes schemars' bare `"$ref": "#"` for a self-recursive root type.
                unhandled("a $ref outside #/$defs/");
            };
            let Some(target) = ctx.defs.get(name) else {
                unhandled("a $ref to a missing $defs entry");
            };
            // Neither `$ref` nor `anyOf` consumes payload, so together they can form a
            // non-decreasing cycle — schemars emits exactly that for `struct X(Option<Box<X>>)`,
            // and unguarded recursion would overflow the stack, aborting the whole test binary
            // rather than failing one test. Every other descent is payload-driven and self-bounding.
            //
            // Stopping is necessary but must not be quiet: a finite payload under mutually
            // recursive `$defs` would otherwise skip every occurrence after the first, which is
            // the silent-skip failure mode this walker exists to refuse. So say so instead.
            if ctx.seen.iter().any(|s| s == name) {
                unhandled("a recursive $defs cycle");
            }
            check_conforms(target, value, &ctx.followed(name));
            return;
        }

        for keyword in ["allOf", "prefixItems", "not", "if"] {
            if node.get(keyword).is_some() {
                unhandled(keyword);
            }
        }
        if node
            .get("additionalProperties")
            .is_some_and(|a| a.is_object())
        {
            unhandled("additionalProperties-as-schema");
        }

        // A node may carry `properties` *and* a combinator, so check its own keys before
        // descending — otherwise a field could ride in unchecked on a sibling keyword.
        let mut checked_here = false;
        if let (Some(emitted_obj), Some(props)) = (
            value.as_object(),
            node.get("properties").and_then(|p| p.as_object()),
        ) {
            check_object(node, props, emitted_obj, ctx);
            checked_here = true;
        }

        // schemars renders `Option<T>` as `anyOf: [T, {"type": "null"}]`. A present value is the
        // non-null branch — descend into it, or the whole Option-shaped surface (`conflict`,
        // `diff`, `guidance`) would go unchecked.
        if let Some(branches) = node.get("anyOf").or_else(|| node.get("oneOf")) {
            let concrete: Vec<_> = branches
                .as_array()
                .map(|b| {
                    b.iter()
                        .filter(|v| v.get("type").and_then(|t| t.as_str()) != Some("null"))
                        .collect()
                })
                .unwrap_or_default();
            match concrete.as_slice() {
                [only] => check_conforms(only, value, ctx),
                // Null-only: `Option`'s None arm, nothing beneath to descend into.
                [] => {}
                // Picking a branch needs real validation, which this walker deliberately is not.
                _ => unhandled("a multi-branch union"),
            }
            return;
        }

        if let Some(items) = node.get("items") {
            if let Some(array) = value.as_array() {
                for (index, element) in array.iter().enumerate() {
                    check_conforms(items, element, &ctx.element(index));
                }
            }
            return;
        }

        // An object payload under a schema describing no properties is an OPEN node: it promises
        // nothing, so there is nothing to check. That is legitimate exactly once — the raw-JSON
        // document — and is otherwise the silent-vacuum failure mode, because a field going opaque
        // upstream (`schemars(with = …)`, a type swapped for `Value`) looks identical to a field
        // that was always meant to be free-form. So openness must be DECLARED: this list is the
        // assertion that exactly one field across the whole engine-tool surface is untyped.
        // Keyed by (tool, path) so the list says what it means: not "any field called document",
        // but this one field of this one tool.
        const OPEN_BY_DESIGN: &[(&str, &str)] = &[
            // The instrument document rides as raw JSON on purpose — the engine is the single
            // validation authority, so the tool surface deliberately does not describe its shape.
            (names::GET_CURRENT_INSTRUMENT, ".document"),
        ];
        // Arrays as well as objects: an array landing here means its node declared no `items`, so
        // every element and everything beneath it would go unchecked. `SwapReport`'s `errors` and
        // `warnings` are exactly that shape, and covering only objects left this hole open on the
        // array axis — the same fail-open bug, one type away.
        let composite = value.is_object() || value.is_array();
        if composite && !checked_here && !OPEN_BY_DESIGN.contains(&(ctx.tool, ctx.path.as_str())) {
            unhandled("an open schema node (nothing declared to check against)");
        }
    }

    /// The two key-set assertions, for one object node.
    fn check_object(
        node: &serde_json::Value,
        props: &serde_json::Map<String, serde_json::Value>,
        emitted_obj: &serde_json::Map<String, serde_json::Value>,
        ctx: &ConformCtx<'_>,
    ) {
        let declared: std::collections::BTreeSet<&str> = props.keys().map(String::as_str).collect();
        let emitted: std::collections::BTreeSet<&str> =
            emitted_obj.keys().map(String::as_str).collect();
        let required: std::collections::BTreeSet<&str> = node["required"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();

        let (tool, case, path) = (ctx.tool, ctx.case, ctx.path.as_str());
        let undeclared: Vec<_> = emitted.difference(&declared).collect();
        assert!(
            undeclared.is_empty(),
            "{tool} / {case}{path}: serialized key(s) {undeclared:?} are not declared by the \
             advertised outputSchema — the tool sends fields it never promised, so a client \
             cannot rely on the schema to know the shape.\n\
             schema properties: {declared:?}\npayload keys: {emitted:?}"
        );

        let promised_but_missing: Vec<_> = required.difference(&emitted).collect();
        assert!(
            promised_but_missing.is_empty(),
            "{tool} / {case}{path}: outputSchema marks {promised_but_missing:?} required, but \
             this payload omits them — a conforming client would reject a response the tool \
             really sends.\n\
             required: {required:?}\npayload keys: {emitted:?}"
        );

        for (name, sub) in props {
            if let Some(sub_value) = emitted_obj.get(name) {
                check_conforms(sub, sub_value, &ctx.child(name));
            }
        }
    }

    /// Where in a payload [`check_conforms`] currently is, for a failure message that names the
    /// tool, the case, and the exact nested field.
    struct ConformCtx<'a> {
        tool: &'a str,
        case: &'a str,
        defs: &'a serde_json::Value,
        path: String,
        /// `$defs` entries already followed on this path, so a self-referential schema terminates
        /// instead of overflowing the stack (which aborts the test binary rather than failing a test).
        seen: Vec<String>,
    }

    impl<'a> ConformCtx<'a> {
        /// The same context one field deeper.
        fn child(&self, field: &str) -> ConformCtx<'a> {
            ConformCtx {
                path: format!("{}.{field}", self.path),
                seen: self.seen.clone(),
                ..*self
            }
        }

        /// The same context at one array element, so a failure names `.errors[2]` rather than
        /// `.errors` — and so a path identifies exactly one node, which the exemption list relies on.
        fn element(&self, index: usize) -> ConformCtx<'a> {
            ConformCtx {
                path: format!("{}[{index}]", self.path),
                seen: self.seen.clone(),
                ..*self
            }
        }

        /// The same context, having followed a `$ref` into `$defs`.
        fn followed(&self, name: &str) -> ConformCtx<'a> {
            let mut seen = self.seen.clone();
            seen.push(name.to_string());
            ConformCtx {
                path: self.path.clone(),
                seen,
                ..*self
            }
        }
    }

    /// Assert that a payload a tool really returns is described by the `outputSchema` that tool
    /// really advertises — top level *and* every nested sub-schema.
    ///
    /// Two derives read the same struct — `Serialize` decides what goes on the wire, `JsonSchema`
    /// decides what we *promise* goes on the wire — and they diverge exactly where attributes are
    /// involved (`#[serde(flatten)]`, `skip_serializing_if`, `rename`, `schemars(skip)`). MCP
    /// requires `structuredContent` to conform to the declared schema, so a divergence breaks
    /// every conforming client while every other test stays green.
    ///
    /// Deliberately NOT a snapshot: nothing here records what the schema *is*. Renaming a type,
    /// re-wording a doc comment, adding a field, or reordering properties all stay green — those
    /// are API changes, not defects. It goes red only when the promise and the payload disagree.
    fn assert_payload_conforms<T: Serialize>(tool: &str, case: &str, payload: &T) {
        let schema = advertised_output_schema(tool);
        let value = serde_json::to_value(payload).expect("payload as value");
        let defs = schema
            .get("$defs")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        check_conforms(
            &schema,
            &value,
            &ConformCtx {
                tool,
                case,
                defs: &defs,
                path: String::new(),
                seen: Vec::new(),
            },
        );
    }

    #[test]
    fn engine_tool_payloads_conform_to_their_advertised_output_schemas() {
        // Every engine tool that returns a structured payload, in every shape it can return one.
        // The engine::SwapResult branches are checked separately because `flatten` +
        // `skip_serializing_if` make them structurally different objects, and `engine_status` is
        // checked both ways because `guidance` is its skip_serializing_if field.
        assert_payload_conforms(
            names::SWAP_INSTRUMENT,
            "clean install",
            &engine::SwapResult::installed(SwapReport {
                report: Report {
                    ok: true,
                    errors: vec![],
                    warnings: vec![],
                },
                content_hash: "00c0ffee".to_string(),
                diff: Some(DiffSummary {
                    survived: 1,
                    state_reset: vec!["/osc".to_string()],
                    added: vec![],
                    removed: vec![],
                }),
            }),
        );
        assert_payload_conforms(
            names::SWAP_INSTRUMENT,
            "validation failure",
            &engine::SwapResult::installed(SwapReport {
                report: Report {
                    ok: false,
                    errors: vec![Diag {
                        node: Some("/osc".to_string()),
                        port: None,
                        message: "unknown operator".to_string(),
                    }],
                    warnings: vec![],
                },
                content_hash: "00c0ffee".to_string(),
                diff: None,
            }),
        );
        assert_payload_conforms(
            names::SWAP_INSTRUMENT,
            "expect-guard miss",
            &engine::SwapResult::conflict(Conflict {
                expected: "0badc0de".to_string(),
                actual: "00c0ffee".to_string(),
            }),
        );
        assert_payload_conforms(
            names::GET_CURRENT_INSTRUMENT,
            "installed document",
            &engine::CurrentInstrument {
                source: Some("voices/t.json".to_string()),
                content_hash: "00c0ffee".to_string(),
                projection: "instrument t · format 3 · 0 nodes".to_string(),
            },
        );
        assert_payload_conforms(
            names::GET_ENGINE_DIAGNOSTICS,
            "counters",
            &DiagnosticsReport::default(),
        );
        assert_payload_conforms(
            names::SEND_LIVE_CONTROLS,
            "queued",
            &engine::SendOutput { sent: 2 },
        );
        for (case, guidance) in [
            ("unreachable", Some(ENGINE_UNREACHABLE_GUIDANCE.to_string())),
            ("reachable", None),
        ] {
            assert_payload_conforms(
                names::GET_ENGINE_STATUS,
                case,
                &engine::EngineStatus {
                    reachable: guidance.is_none(),
                    endpoints: engine::StatusEndpoints {
                        structure: DEFAULT_STRUCTURE_ADDR.to_string(),
                    },
                    sidecar: engine::SidecarInfo {
                        version: env!("CARGO_PKG_VERSION").to_string(),
                        format_version: authoring::FORMAT_VERSION,
                    },
                    guidance,
                },
            );
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
    fn resolve_checkout_path_prefers_the_override_then_the_default() {
        // Called with explicit args, so no process env is mutated: this cannot flake, and it keeps
        // a genuine direct caller for the pure signature that makes that possible.
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
        // Driven through the production method, so a `resolve_path` reading a hardcoded var instead
        // of `self.env`, or a row with a cross-wired `env`, fails here.
        //
        // `set_var`/`remove_var` are process-global and cargo runs tests as threads in one process,
        // so this is only safe because these three REUBEN_* vars are read by NO other inline test.
        // Per row the env is mutated panic-safely: set, capture BOTH outcomes into locals, and
        // `remove_var` BEFORE any assertion runs, so a failed assert cannot leak a var sideways.
        for (i, entry) in RESOURCES.iter().enumerate() {
            let override_path = format!("/opt/reuben/resource-{i}.md");

            std::env::set_var(entry.env, &override_path);
            let got_override = entry.resolve_path();
            std::env::remove_var(entry.env);
            let got_default = entry.resolve_path();

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
    fn every_served_resource_sends_a_model_only_to_verbs_the_roster_serves() {
        // The guides are the largest model-facing prose in the repo and the door reads them from
        // disk at request time, so nothing about them is compile-coupled to anything: the authoring
        // guide alone names 13 roster verbs. Driving the scan off RESOURCES rather than a list of
        // paths is what makes a resource added later scanned by default instead of by remembering.
        for entry in RESOURCES {
            let path = entry.resolve_path();
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read the {} at {}: {e}", entry.noun, path.display()));
            assert_eq!(
                reuben_api::tools::unserved_verbs(&text),
                Vec::<&str>::new(),
                "{} sends a model to a verb no contract serves",
                entry.uri
            );
            assert_eq!(
                reuben_api::tools::unserved_verbs(entry.description),
                Vec::<&str>::new(),
                "{}'s advertised description sends a model to a verb no contract serves",
                entry.uri
            );
        }
    }

    #[test]
    fn the_instructions_send_a_model_only_to_verbs_the_roster_serves() {
        // The gist names six verbs and is handed to every model that connects, so it is prose with
        // the same failure mode as an advertised sentence — and `stamp_window_prose`'s refusal
        // cannot see it, because that compares route keys and never reads inside a string. A verb
        // fully retired upstream (contract and route together) leaves the rest of this door
        // compiling and every other test green while the gist still tells a model to call it.
        assert_eq!(
            reuben_api::tools::unserved_verbs(INSTRUCTIONS),
            Vec::<&str>::new(),
            "the server instructions send a model to a verb no contract serves"
        );
    }

    #[test]
    fn resource_table_is_self_consistent() {
        // The "new resource = one row" guard: a copy-pasted row that forgot to update its URI, env
        // override, or default path would silently alias another resource's.
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
        // The assembled prose is unguarded by the integration tests, which only do substring
        // `.contains()` checks — so pin the exact 3-resource wording here.
        assert_eq!(
            served_resource_uris(),
            format!(
                "{GUIDE_RESOURCE_URI}, {VOCABULARY_RESOURCE_URI}, and {LIBRARY_INDEX_RESOURCE_URI}"
            ),
            "the served-URI list must read as an Oxford list for the current 3-resource roster"
        );
    }

    #[test]
    fn swap_instrument_result_serializes_report_hash_and_diff() {
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
            diff: Some(DiffSummary {
                survived: 0,
                state_reset: vec!["/osc".to_string()],
                added: vec!["/delay".to_string()],
                removed: vec![],
            }),
        };
        let v = serde_json::to_value(engine::SwapResult::installed(report)).expect("serialize");
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
    fn get_engine_status_dead_engine_is_not_iserror_and_has_guidance() {
        // engine_status answers "reachable?", so it is NEVER isError — even on a dead
        // engine it reports the down state (reachable:false + guidance) as its deliverable, with the
        // endpoints and sidecar identity still filled in.
        let result = block_on(server_with(FakeTransport::unreachable()).get_engine_status())
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
            s["endpoints"]["structure"].is_string(),
            "the endpoint is reported even when down: {s}"
        );
        assert_eq!(
            s["sidecar"]["format_version"],
            serde_json::json!(authoring::FORMAT_VERSION),
            "the sidecar reports the supported instrument format_version: {s}"
        );
    }

    #[test]
    fn get_engine_status_reachable_reports_true_and_omits_guidance() {
        // The live branch: reachable:true and no guidance key (guidance appears only when down).
        let result = block_on(server_with(FakeTransport::reachable()).get_engine_status())
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
    fn send_live_controls_rejects_empty_messages() {
        // (a) The advertised input schema declares minItems:1.
        let router_server = ReubenServer::new();
        let send = router_server
            .tool_router
            .list_all()
            .into_iter()
            .find(|t| t.name == names::SEND_LIVE_CONTROLS)
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
                .send_live_controls(Parameters(engine::SendLiveControls { messages: vec![] })),
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
    fn swap_instrument_expect_mismatch_returns_conflict_no_install() {
        // An expect-guard miss is an ORDINARY result (the guard
        // guarding, not the tool failing), NOT isError; nothing is installed (ok:false, no diff),
        // and the conflict names both hashes field-for-field so the model reconciles.
        let fake = FakeTransport::reachable().with_swap(Response::Conflict(Conflict {
            expected: "0badc0de".to_string(),
            actual: "00c0ffee".to_string(),
        }));
        let result = block_on(server_with(fake).swap_instrument(Parameters(
            engine::SwapInstrument {
                path: "instruments/pad.json".to_string(),
                expect: Some("0badc0de".to_string()),
            },
        )))
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
        // The contract `SwapReport::rejected` exists to own: content_hash names what KEEPS
        // PLAYING (the conflict's `actual`), never the `expected` the client asked for. Reporting
        // `expected` here would hand the model a hash that is not installed, so its next
        // expect-guarded swap conflicts again — an unbreakable loop.
        assert_eq!(
            s["content_hash"],
            serde_json::json!("00c0ffee"),
            "a rejected swap reports the hash still playing, not the one asked for: {s}"
        );
    }

    #[test]
    fn swap_instrument_relays_diff_summary_verbatim() {
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
            diff: Some(DiffSummary {
                survived: 0,
                state_reset: vec!["/osc".to_string()],
                added: vec![],
                removed: vec![],
            }),
        };
        let fake = FakeTransport::reachable().with_swap(Response::SwapReport(report));
        let result = block_on(server_with(fake).swap_instrument(Parameters(
            engine::SwapInstrument {
                path: "instruments/pad.json".to_string(),
                expect: None,
            },
        )))
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
    fn swap_instrument_unreachable_is_iserror() {
        // Act-then-map on the mutating verb: a down engine is the fail-fast isError.
        let result = block_on(server_with(FakeTransport::unreachable()).swap_instrument(
            Parameters(engine::SwapInstrument {
                path: "instruments/pad.json".to_string(),
                expect: None,
            }),
        ))
        .expect("swap returns a result");
        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("reuben play"));
    }

    #[test]
    fn send_live_controls_puts_the_whole_batch_on_the_structure_channel_in_one_exchange() {
        // The batch crosses as ONE `send` request — not one per message — and the assertion is on
        // what the fake parsed back OFF THE WIRE, so a serialization regression is caught rather
        // than a counter being trusted. The JSON args re-type on the way: `1200.0` is a float atom,
        // `69`/`1` are integers, `"up"` a symbol.
        let params = engine::SendLiveControls {
            messages: vec![
                engine::ControlSendMessage {
                    address: "/voice1/cutoff".to_string(),
                    args: vec![serde_json::json!(1200.0)],
                },
                engine::ControlSendMessage {
                    address: "/voice1/notes".to_string(),
                    args: vec![serde_json::json!(69), serde_json::json!(1)],
                },
                engine::ControlSendMessage {
                    address: "/lfo/shape".to_string(),
                    args: vec![serde_json::json!("up")],
                },
            ],
        };
        let (transport, sent) = FakeTransport::recording_send();
        let result = block_on(server_with(transport).send_live_controls(Parameters(params)))
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
            serde_json::json!(3),
            "the tool relays the engine's own ack count"
        );

        assert_eq!(
            *sent.lock().expect("sent log"),
            vec![
                ControlMessage {
                    address: "/voice1/cutoff".to_string(),
                    args: vec![ControlArg::F32(1200.0)],
                },
                ControlMessage {
                    address: "/voice1/notes".to_string(),
                    args: vec![ControlArg::I32(69), ControlArg::I32(1)],
                },
                ControlMessage {
                    address: "/lfo/shape".to_string(),
                    args: vec![ControlArg::Str("up".to_string())],
                },
            ],
            "every message crosses in order, each arg as the atom its JSON spelling names"
        );
    }

    #[test]
    fn send_live_controls_rejects_a_number_that_cannot_survive_the_f32_wire() {
        // 1e39 saturates to f32 infinity, which serde_json writes as `null` — a value no
        // `ControlArg` accepts, so an unguarded conversion would put an unparseable line on the wire
        // and take the WHOLE batch down with an opaque "did not match any variant". The guard turns
        // that into a named error about the one bad argument, and nothing is sent.
        let params = engine::SendLiveControls {
            messages: vec![
                engine::ControlSendMessage {
                    address: "/voice1/cutoff".to_string(),
                    args: vec![serde_json::json!(1e39)],
                },
                engine::ControlSendMessage {
                    address: "/voice1/gain".to_string(),
                    args: vec![serde_json::json!(0.5)],
                },
            ],
        };
        let (transport, sent) = FakeTransport::recording_send();
        let result = block_on(server_with(transport).send_live_controls(Parameters(params)))
            .expect("result");
        assert_eq!(result.is_error, Some(true), "{result:?}");
        assert!(
            first_text(&result).contains("/voice1/cutoff"),
            "the error names the offending message: {}",
            first_text(&result)
        );
        assert!(
            sent.lock().expect("sent log").is_empty(),
            "the good message must not go out on its own — the batch is rejected whole"
        );
    }

    #[test]
    fn send_live_controls_rejects_a_batch_over_the_shared_limit() {
        // The engine applies a batch inside one render callback, so the cap is an RT bound. The tool
        // stops an over-long batch before it reaches the wire, naming the limit so a model can split.
        let params = engine::SendLiveControls {
            messages: (0..engine::MAX_SEND_BATCH + 1)
                .map(|i| engine::ControlSendMessage {
                    address: format!("/voice1/p{i}"),
                    args: vec![serde_json::json!(0.5)],
                })
                .collect(),
        };
        let (transport, sent) = FakeTransport::recording_send();
        let result = block_on(server_with(transport).send_live_controls(Parameters(params)))
            .expect("result");
        assert_eq!(result.is_error, Some(true), "{result:?}");
        assert!(
            first_text(&result).contains(&engine::MAX_SEND_BATCH.to_string()),
            "the error names the limit: {}",
            first_text(&result)
        );
        assert!(sent.lock().expect("sent log").is_empty());
    }

    #[test]
    fn send_live_controls_schema_advertises_the_shared_batch_limit() {
        // `schemars` takes a literal, not a const, so the advertised maxItems and the shared
        // MAX_SEND_BATCH the engine enforces are two spellings of one number. Pin them together:
        // drifting them apart would advertise a limit the engine does not honor.
        let server = ReubenServer::new();
        let tools = server.tool_router.list_all();
        let send = tools
            .iter()
            .find(|t| t.name == names::SEND_LIVE_CONTROLS)
            .expect("send is registered");
        let schema = serde_json::to_value(&send.input_schema).expect("input schema to value");
        let messages = &schema["properties"]["messages"];
        assert_eq!(
            messages["maxItems"],
            serde_json::json!(engine::MAX_SEND_BATCH),
            "advertised maxItems must equal the engine-enforced limit: {schema}"
        );
        assert_eq!(messages["minItems"], serde_json::json!(1), "{schema}");
    }

    #[test]
    fn send_live_controls_unreachable_is_iserror() {
        // Act-then-map, no probe: the batch converts fine, and the exchange ITSELF reports the down
        // engine — there is no separate ping to fail first.
        let params = engine::SendLiveControls {
            messages: vec![engine::ControlSendMessage {
                address: "/voice1/cutoff".to_string(),
                args: vec![serde_json::json!(1.0)],
            }],
        };
        let result = block_on(
            server_with(FakeTransport::unreachable()).send_live_controls(Parameters(params)),
        )
        .expect("result");
        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("reuben play"));
    }

    #[test]
    fn send_live_controls_rejects_a_non_scalar_argument() {
        // Args are number | string; a nested array is a can't-do-the-job error, caught before
        // anything reaches the engine (and without needing one). Kept as the tool's own crafted
        // error rather than an rmcp deserialization failure, so the model is told what was wrong.
        let params = engine::SendLiveControls {
            messages: vec![engine::ControlSendMessage {
                address: "/voice1/cutoff".to_string(),
                args: vec![serde_json::json!([1, 2, 3])],
            }],
        };
        let (transport, sent) = FakeTransport::recording_send();
        let result = block_on(server_with(transport).send_live_controls(Parameters(params)))
            .expect("result");
        assert_eq!(
            result.is_error,
            Some(true),
            "an unsupported argument must be isError: {result:?}"
        );
        assert!(
            sent.lock().expect("sent log").is_empty(),
            "a rejected batch reaches the engine not at all — not partially"
        );
    }

    #[test]
    fn get_current_instrument_projects_the_installed_graph_and_never_returns_it() {
        // The tool answers "what is playing?" with a projection of what the ENGINE holds, plus the
        // source and hash to reconcile against — the document itself stops at this door.
        let doc = serde_json::json!({
            "format_version": 3,
            "instrument": "warm",
            "nodes": [{ "type": "oscillator", "address": "/osc", "inputs": { "freq": 220.0 } }],
        });
        let snapshot = DocumentSnapshot {
            document: doc.clone(),
            content_hash: "00c0ffee".to_string(),
            source: Some("voices/warm.json".to_string()),
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
        assert_eq!(s["source"], serde_json::json!("voices/warm.json"));
        let projection = s["projection"].as_str().expect("a rendered projection");
        assert!(
            projection.contains("/osc") && projection.contains("oscillator"),
            "the projection is of the installed graph: {projection}"
        );
        assert_eq!(
            s.get("document"),
            None,
            "the installed document must never ride back to the model"
        );
    }

    #[test]
    fn get_engine_diagnostics_returns_the_four_counters() {
        let report = DiagnosticsReport {
            output_xruns: 2,
            input_ring_underruns: 480,
            input_ring_overruns: 0,
            input_ring_producer_drops: 96,
        };
        let result = block_on(
            server_with(FakeTransport::reachable().with_diagnostics(report))
                .get_engine_diagnostics(),
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
    fn get_engine_diagnostics_unreachable_is_iserror() {
        let result = block_on(server_with(FakeTransport::unreachable()).get_engine_diagnostics())
            .expect("result");
        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("reuben play"));
    }

    #[test]
    fn no_advertised_schema_carries_an_instrument_document() {
        // The no-document-in-context claim as a build-time property rather than a cleanup that
        // happened once. It bites on the field NAME, walked over every input and output schema
        // including `$defs`, because that is how the retired arms spelled it and how a
        // re-introduction would spell it too. see rules: agent-mcp
        fn walk(node: &serde_json::Value, tool: &str, whose: &str, path: &str) {
            match node {
                serde_json::Value::Object(map) => {
                    if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                        for name in props.keys() {
                            assert_ne!(
                                name, "document",
                                "{tool}'s {whose} schema carries a `document` field at {path} — no \
                                 arm moves instrument JSON; name the document by `source` and \
                                 answer with a projection (see rules: agent-mcp)"
                            );
                        }
                    }
                    for (key, child) in map {
                        walk(child, tool, whose, &format!("{path}/{key}"));
                    }
                }
                serde_json::Value::Array(items) => {
                    for (i, child) in items.iter().enumerate() {
                        walk(child, tool, whose, &format!("{path}/{i}"));
                    }
                }
                _ => {}
            }
        }

        // Teeth: a walker that cannot find a `document` field would pass this test forever. Prove it
        // bites on the shape a real schema has — nested under `$defs`, as `scaffold_instrument`'s
        // retired output was — before trusting it on the roster.
        let planted = serde_json::json!({
            "type": "object",
            "properties": { "result": { "$ref": "#/$defs/Snapshot" } },
            "$defs": { "Snapshot": {
                "type": "object",
                "properties": { "document": { "type": "object" }, "hash": { "type": "string" } },
            } },
        });
        assert!(
            std::panic::catch_unwind(|| walk(&planted, "planted", "output", "")).is_err(),
            "the walk must catch a document field nested in $defs"
        );

        let server = ReubenServer::new();
        let tools = server.tool_router.list_all();
        // The walk below proves nothing over an empty router. That the router *is* the roster is
        // not asserted here — `stamp_window_prose` refuses to construct this server otherwise, so
        // a length comparison would be a tautology dressed as a check.
        assert!(
            !tools.is_empty(),
            "the walk below must have a roster to cover"
        );
        for tool in tools {
            let name = tool.name.to_string();
            walk(
                &serde_json::to_value(&tool.input_schema).expect("input schema as value"),
                &name,
                "input",
                "",
            );
            if let Some(output) = &tool.output_schema {
                walk(
                    &serde_json::to_value(output).expect("output schema as value"),
                    &name,
                    "output",
                    "",
                );
            }
        }
    }

    /// A minimal valid instrument the document-tool door tests edit on disk.
    fn seed_instrument() -> &'static str {
        r#"{
            "format_version": 3,
            "instrument": "door-test",
            "nodes": [ { "type": "oscillator", "address": "/osc", "inputs": { "freq": 220.0 } } ]
        }"#
    }

    /// End-to-end through the FS door: a document verb interprets the opaque `source` path, edits the
    /// file, and returns the `EditResult`. Proves the door's path rooting, write-through, and the
    /// single `EditResult` output shape — none of which the core-level tests (over `MemoryResolver`)
    /// exercise.
    #[test]
    fn document_verb_edits_the_file_through_the_fs_door() {
        let dir = std::env::temp_dir().join("reuben_mcp_edit_door_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inst.json");
        std::fs::write(&path, seed_instrument()).unwrap();

        let server = ReubenServer::new();
        let result = block_on(server.set_instrument_input(Parameters(
            authoring::SetInstrumentInput {
                source: path.display().to_string(),
                address: "/osc".to_string(),
                input: "freq".to_string(),
                value: serde_json::json!(440.0),
                expect: None,
            },
        )))
        .expect("set_instrument_input returns a result");

        assert_ne!(
            result.is_error,
            Some(true),
            "a valid edit is not isError: {result:?}"
        );
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(
            s["written"],
            serde_json::json!(true),
            "the edit was written: {s}"
        );
        assert_eq!(s["report"]["ok"], serde_json::json!(true));
        assert!(s["hash"].as_str().is_some_and(|h| !h.is_empty()));

        // The file on disk actually changed.
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            on_disk["nodes"][0]["inputs"]["freq"],
            serde_json::json!(440.0)
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The door-side `expect` guard: a mismatched hash rejects the write as an ordinary result
    /// (nothing changed), reporting the real current hash to reconcile against.
    #[test]
    fn document_verb_expect_guard_rejects_a_stale_write() {
        let dir = std::env::temp_dir().join("reuben_mcp_edit_expect_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inst.json");
        std::fs::write(&path, seed_instrument()).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();

        let server = ReubenServer::new();
        let result = block_on(server.set_instrument_input(Parameters(
            authoring::SetInstrumentInput {
                source: path.display().to_string(),
                address: "/osc".to_string(),
                input: "freq".to_string(),
                value: serde_json::json!(440.0),
                expect: Some("deadbeefdeadbeef".to_string()),
            },
        )))
        .expect("returns a result");

        assert_ne!(
            result.is_error,
            Some(true),
            "a guard miss is ordinary, not isError"
        );
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(
            s["written"],
            serde_json::json!(false),
            "a stale write is refused: {s}"
        );
        // The file is untouched.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        // The guard tells the caller to re-read and reconcile, so it hands back the current view
        // rather than an empty zoom that would cost a second round trip to fill.
        let zoom = s["zoom"].as_str().expect("a zoom string");
        assert!(
            zoom.contains("/osc"),
            "the guard echoes the current node index: {zoom:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A can't-do-the-job precondition (no such node) is `isError`, distinct from a rejected edit.
    #[test]
    fn document_verb_missing_target_is_iserror() {
        let dir = std::env::temp_dir().join("reuben_mcp_edit_target_test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("inst.json");
        std::fs::write(&path, seed_instrument()).unwrap();

        let server = ReubenServer::new();
        let result = block_on(server.set_instrument_input(Parameters(
            authoring::SetInstrumentInput {
                source: path.display().to_string(),
                address: "/ghost".to_string(),
                input: "freq".to_string(),
                value: serde_json::json!(1.0),
                expect: None,
            },
        )))
        .expect("returns a result");
        assert_eq!(
            result.is_error,
            Some(true),
            "a missing target is isError: {result:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
