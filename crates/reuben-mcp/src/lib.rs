//! reuben-mcp — the per-conversation MCP stdio sidecar.
//!
//! A [`ServerHandler`] with a tool router over the [`reuben_api::tools::CONTRACTS`] roster. Only
//! `describe_operators` answers in-process, through [`reuben_api::authoring`]: it reads no file.
//! Every other tool reaches a user-owned `reuben play` through [`reuben_api::engine`], over the
//! loopback transport in [`EngineLink`]. The engine verbs keep their own requests; every document
//! read and edit is a `call` the engine runs against the instrument files it owns, so this door
//! never reads or writes one.
//!
//! What is left here is the MCP-shaped part and only that: the roster, the transport, and the
//! two-way map between a window answer and a `CallToolResult`. The argument shapes, the result
//! shapes, the guards and the glosses belong to the window, so a second door inherits them rather
//! than reimplementing them.
//!
//! That includes the per-tool `description` sentences, which the window owns too. rmcp's `#[tool]`
//! does take a string **literal** there and cannot name a const, so the attribute is left off and
//! `stamp_window_prose` writes the window's sentence onto the built router instead — the same
//! prose the CLI and the browser read, rather than a copy per door.
//!
//! The `name` argument is literal-only for the same reason, so the roster spelling is written out
//! once per tool here — redundantly, since rmcp defaults a route's name to the method's own ident
//! and every method is already named for its verb, but written anyway so the wire name is read off
//! the attribute rather than inferred from a default. Nothing about that literal is coupled to the
//! roster, and `stamp_window_prose`'s two assertions are what stand in for the coupling: at
//! construction, in both directions, refusing to start rather than serve a surface that is not the
//! roster.
//!
//! Everywhere the door names a contract as a *value* rather than as a route key it writes
//! [`reuben_api::tools::names`](mod@reuben_api::tools::names) instead, so the window dropping that
//! contract is a compile error here. Non-test source does so for each verb it relays as a `call`,
//! and for the roster verbs it does not serve.
//!
//! The one thing left that is genuinely this door's is the socket: `reuben-mcp` reaches a
//! *separate process*, so it supplies a loopback TCP transport where an in-process host supplies
//! none at all.

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
use reuben_api::tools::names;
use serde::Serialize;

mod engine_link;
pub use engine_link::EngineLink;
pub use reuben_api::engine::{TcpTransport, DEFAULT_CONNECT_TIMEOUT};

/// The window's engine-half surface, re-exported for a test or a caller driving this door: the
/// channel a fake transport plugs into, and the payloads it carries.
pub use reuben_api::engine::{
    Channel, ChannelError, Conflict, DocumentSnapshot, SwapOutcome, Transport,
    ENGINE_UNREACHABLE_GUIDANCE,
};

/// The tool surface this door advertises, in roster order — the exact spellings on `tools/list`,
/// so the wire surface can only change by changing the roster.
pub fn tool_names() -> Vec<&'static str> {
    served_contracts().map(|c| c.name).collect()
}

/// The contracts this door routes: the roster, less host-only verbs.
fn served_contracts() -> impl Iterator<Item = &'static reuben_api::tools::Contract> {
    reuben_api::tools::CONTRACTS
        .iter()
        .filter(|c| c.kind != reuben_api::tools::ContractKind::Host)
}

pub use reuben_api::engine::STRUCTURE_ADDR_ENV;

/// The `reuben://guide/authoring` resource URI: the MCP lane's cut of the authoring guide,
/// `docs/agents/authoring.md`.
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
/// resources rather than restating them.
const INSTRUCTIONS: &str = "reuben authoring sidecar. The instrument document is the durable \
     truth, and what it says is what plays. **Never read or write an instrument file \
     yourself** — name it and let these tools do it: `describe_instrument` reads its structure, \
     and the `*_instrument_*` verbs edit it one change at a time. Start `reuben play` in another \
     terminal first: every instrument tool asks it, and fails fast until it is reachable. It \
     offers the instruments in its library directory (`reuben play --instrument-root <dir>`, \
     else `instruments/` where it starts) and beside the one it plays (`reuben play <file>`), \
     and `new_instrument` creates only there. The loop: `list_instruments` gives the names it \
     offers (or `new_instrument` lands a \
     guaranteed-valid seed at a new one); `open_instrument` opens one, and you must open an \
     instrument before editing it; then each accepted edit is live at once — there is no separate \
     step to make it heard. Every edit re-validates the whole instrument and is refused, with \
     nothing written, if it would break it — so while a document has errors, every edit that \
     leaves them is refused. A document that does not load still opens, with nothing playing: an \
     edit it accepts is written, and playing starts once it loads. A rejection lists under \
     `pre_existing` the errors it can show were already there; fix those first. Validation \
     reports one error at a time, so read a rejection's `notes` too. \
     `send_live_controls` auditions a value without committing it, and the next edit resets it, \
     so fold a keeper into the document with an edit. Read `reuben://guide/authoring` \
     for the type system, wiring rules, instrument format, and the authoring loop. Read \
     `reuben://guide/vocabulary` for the word→move table translating intent language (\"warmer\", \
     \"busier\", \"sadder\") into parameter moves. Read `reuben://guide/library-index` for the \
     available instruments to reuse by reference through a `subpatch` node.";

/// Default absolute path to the authoring guide (`docs/agents/authoring.md`). Only the *path* is
/// compile-time — the file itself is read at request time — and it is valid only in the checkout
/// the sidecar was built in; a deploy outside one overrides it with [`AUTHORING_GUIDE_ENV`].
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
/// binary whose `AUTHORING_GUIDE_PATH` points into a build checkout that need not exist where it
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
    /// The lane whose cut of the file is served, for a file tagged by lane; `None` serves the
    /// file whole.
    lane: Option<reuben_guide::Lane>,
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
        let text = match self.lane {
            Some(lane) => reuben_guide::lane_cut(&text, lane).map_err(|e| {
                McpError::internal_error(
                    format!("failed to cut the {} at {}: {e}", self.noun, path.display()),
                    None,
                )
            })?,
            None => text,
        };
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
        lane: Some(reuben_guide::Lane::Mcp),
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
        lane: None,
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
        lane: None,
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
/// `describe_operators` is always available. Every other tool reaches a user-owned `reuben play`
/// through [`EngineLink`] and fails fast when it is unreachable, except `get_engine_status`, which
/// answers "reachable?" and so is never itself an error.
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

    // --- In-process: the one tool that reads no file ---------------------------------------------

    /// List the operator set. Engine-free, so an agent can ground itself before an engine runs.
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

    // --- Document reads: a `call` to the engine, which owns the files -----------------------------

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
        answered(engine::call_tool(
            names::DESCRIBE_INSTRUMENT,
            &p,
            self.engine.structure(),
        ))
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
        answered(engine::call_tool(
            names::VALIDATE_INSTRUMENT,
            &p,
            self.engine.structure(),
        ))
    }

    // --- Engine tools: reach a user-owned `reuben play` through the channel seam ----------------

    // Each is one delegation, like the document verbs: the batch bounds, the argument
    // conversion, the unreachable/other split, the guard-miss shape and the glosses all live behind
    // `engine`. What this door still owns is the socket under the channel, the advertised schema,
    // and the isError decision.

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
        name = "open_instrument",
        output_schema = rmcp::handler::server::tool::schema_for_output::<engine::SwapResult>()
            .expect("SwapResult is an object schema")
    )]
    async fn open_instrument(
        &self,
        Parameters(p): Parameters<engine::OpenInstrument>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::open_instrument(&p, self.engine.structure()))
    }

    #[tool(
        name = "list_instruments",
        output_schema = rmcp::handler::server::tool::schema_for_output::<engine::InstrumentList>()
            .expect("InstrumentList is an object schema")
    )]
    async fn list_instruments(
        &self,
        Parameters(p): Parameters<engine::ListInstruments>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::list_instruments(&p, self.engine.structure()))
    }

    #[tool(
        name = "get_current_instrument",
        output_schema =
            rmcp::handler::server::tool::schema_for_output::<engine::CurrentInstrument>()
                .expect("CurrentInstrument is an object schema")
    )]
    async fn get_current_instrument(&self) -> Result<CallToolResult, McpError> {
        answered(engine::current_instrument(self.engine.structure()))
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

    #[tool(
        name = "get_engine_meters",
        output_schema = rmcp::handler::server::tool::schema_for_output::<engine::MetersReport>()
            .expect("MetersReport is an object schema")
    )]
    async fn get_engine_meters(&self) -> Result<CallToolResult, McpError> {
        answered(engine::get_engine_meters(self.engine.structure()))
    }

    // --- Document edits: a `call` to the engine, which owns the files -----------------------------
    //
    // One roster entry per window verb, and nothing else. The `expect` guard, the write-iff-valid
    // pipeline, the swap of an accepted edit, the projection echo and the one-line gloss all run on
    // the engine's side, and the advertised sentence lives behind `authoring`. What the door still
    // owns is the roster spelling, the advertised schema, and the isError decision.

    #[tool(
        name = "new_instrument",
        output_schema = edit_result_schema()
    )]
    async fn new_instrument(
        &self,
        Parameters(p): Parameters<authoring::NewInstrument>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::NEW_INSTRUMENT,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "set_instrument_name",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_name(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentName>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::SET_INSTRUMENT_NAME,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "set_instrument_description",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_description(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentDescription>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::SET_INSTRUMENT_DESCRIPTION,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "add_instrument_node",
        output_schema = edit_result_schema()
    )]
    async fn add_instrument_node(
        &self,
        Parameters(p): Parameters<authoring::AddInstrumentNode>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::ADD_INSTRUMENT_NODE,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "remove_instrument_node",
        output_schema = edit_result_schema()
    )]
    async fn remove_instrument_node(
        &self,
        Parameters(p): Parameters<authoring::RemoveInstrumentNode>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::REMOVE_INSTRUMENT_NODE,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "rename_instrument_node",
        output_schema = edit_result_schema()
    )]
    async fn rename_instrument_node(
        &self,
        Parameters(p): Parameters<authoring::RenameInstrumentNode>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::RENAME_INSTRUMENT_NODE,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "set_instrument_node_description",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_node_description(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentNodeDescription>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::SET_INSTRUMENT_NODE_DESCRIPTION,
            &p,
            self.engine.structure(),
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
        answered(engine::call_tool(
            names::SET_INSTRUMENT_INPUT,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "set_instrument_inputs_by_intent",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_inputs_by_intent(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentInputsByIntent>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::SET_INSTRUMENT_INPUTS_BY_INTENT,
            &p,
            self.engine.structure(),
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
        answered(engine::call_tool(
            names::WIRE_INSTRUMENT_INPUT,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "unwire_instrument_input",
        output_schema = edit_result_schema()
    )]
    async fn unwire_instrument_input(
        &self,
        Parameters(p): Parameters<authoring::UnwireInstrumentInput>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::UNWIRE_INSTRUMENT_INPUT,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "set_instrument_constant",
        output_schema = edit_result_schema()
    )]
    async fn set_instrument_constant(
        &self,
        Parameters(p): Parameters<authoring::SetInstrumentConstant>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::SET_INSTRUMENT_CONSTANT,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "remove_instrument_constant",
        output_schema = edit_result_schema()
    )]
    async fn remove_instrument_constant(
        &self,
        Parameters(p): Parameters<authoring::RemoveInstrumentConstant>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::REMOVE_INSTRUMENT_CONSTANT,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "add_instrument_interface_input",
        output_schema = edit_result_schema()
    )]
    async fn add_instrument_interface_input(
        &self,
        Parameters(p): Parameters<authoring::AddInstrumentInterfaceInput>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::ADD_INSTRUMENT_INTERFACE_INPUT,
            &p,
            self.engine.structure(),
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
        answered(engine::call_tool(
            names::ADD_INSTRUMENT_INTERFACE_OUTPUT,
            &p,
            self.engine.structure(),
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
        answered(engine::call_tool(
            names::REMOVE_INSTRUMENT_INTERFACE_INPUT,
            &p,
            self.engine.structure(),
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
        answered(engine::call_tool(
            names::REMOVE_INSTRUMENT_INTERFACE_OUTPUT,
            &p,
            self.engine.structure(),
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
        answered(engine::call_tool(
            names::SET_INSTRUMENT_INTERFACE_INPUT_META,
            &p,
            self.engine.structure(),
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
        answered(engine::call_tool(
            names::SET_INSTRUMENT_INTERFACE_OUTPUT_META,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "clear_instrument_interface_meta",
        output_schema = edit_result_schema()
    )]
    async fn clear_instrument_interface_meta(
        &self,
        Parameters(p): Parameters<authoring::ClearInstrumentInterfaceMeta>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::CLEAR_INSTRUMENT_INTERFACE_META,
            &p,
            self.engine.structure(),
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
        answered(engine::call_tool(
            names::ADD_INSTRUMENT_RESOURCE,
            &p,
            self.engine.structure(),
        ))
    }

    #[tool(
        name = "remove_instrument_resource",
        output_schema = edit_result_schema()
    )]
    async fn remove_instrument_resource(
        &self,
        Parameters(p): Parameters<authoring::RemoveInstrumentResource>,
    ) -> Result<CallToolResult, McpError> {
        answered(engine::call_tool(
            names::REMOVE_INSTRUMENT_RESOURCE,
            &p,
            self.engine.structure(),
        ))
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

    for contract in served_contracts() {
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
    /// An unknown URI is `resource_not_found`.
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
        ChannelLevel, ControlArg, ControlMessage, DiagnosticsReport, DiffSummary, HeldSwap,
        MetersReport, Request, Response, SwapReport, DEFAULT_STRUCTURE_ADDR,
    };
    use reuben_api::tools::names;
    use std::io;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    /// A [`Transport`] answering with canned NDJSON instead of dialing a socket.
    ///
    /// It substitutes only the *bytes on the wire*: the request line is parsed here, so a malformed
    /// one fails the test, and an unconfigured verb returns the same [`io::Error`] a dead socket
    /// would.
    #[derive(Debug, Default)]
    struct FakeTransport {
        ping: Option<Response>,
        document: Option<Response>,
        diagnostics: Option<Response>,
        meters: Option<Response>,
        open: Option<Response>,
        /// The answer to a `call` of each scripted tool.
        calls: Vec<(String, Response)>,
        /// Every request that crossed the wire, in order, parsed back off it.
        requests: Arc<Mutex<Vec<Request>>>,
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

        fn with_open(mut self, response: Response) -> Self {
            self.open = Some(response);
            self
        }

        fn with_call(mut self, tool: &str, response: Response) -> Self {
            self.calls.push((tool.to_string(), response));
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

        fn with_meters(mut self, report: MetersReport) -> Self {
            self.meters = Some(Response::Meters(report));
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

        /// The shared log of every request this transport answers, readable after the transport
        /// has moved into a server.
        fn request_log(&self) -> Arc<Mutex<Vec<Request>>> {
            Arc::clone(&self.requests)
        }
    }

    impl Transport for FakeTransport {
        fn round_trip(&self, line: &str, _read_timeout: Duration) -> io::Result<String> {
            let request = Request::from_ndjson(line)
                .expect("the tool body must put a well-formed request line on the wire");
            self.requests
                .lock()
                .expect("request log")
                .push(request.clone());
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
            let configured = match &request {
                Request::Ping => self.ping.as_ref(),
                Request::GetDocument => self.document.as_ref(),
                Request::GetDiagnostics => self.diagnostics.as_ref(),
                Request::GetMeters => self.meters.as_ref(),
                Request::Open { .. } => self.open.as_ref(),
                Request::Call { tool, .. } => self
                    .calls
                    .iter()
                    .find(|(scripted, _)| scripted == tool)
                    .map(|(_, response)| response),
                Request::SetProbes { .. }
                | Request::List { .. }
                | Request::Library
                | Request::Surface
                | Request::Subscribe => {
                    unreachable!("no tool sends it")
                }
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
        // `open_instrument` is the route that advertises SwapResult. Its branches are checked
        // separately because `flatten` + `skip_serializing_if` make them structurally different
        // objects, and `engine_status` is checked both ways because `guidance` is its
        // skip_serializing_if field.
        assert_payload_conforms(
            names::OPEN_INSTRUMENT,
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
            names::OPEN_INSTRUMENT,
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
            names::OPEN_INSTRUMENT,
            "expect-guard miss",
            &engine::SwapResult::conflict(Conflict {
                expected: "0badc0de".to_string(),
                actual: "00c0ffee".to_string(),
            }),
        );
        assert_payload_conforms(
            names::OPEN_INSTRUMENT,
            "held for the editor's user",
            &engine::SwapResult::held(HeldSwap {
                requested: "instruments/groovebox.json".to_string(),
                playing: Some("instruments/acid-techno.json".to_string()),
                content_hash: "00c0ffee".to_string(),
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
        // `default_path`, not `resolve_path()`: the checked-in guides are the subject, so a host's
        // `REUBEN_*` override would point this at the wrong file — and reading one here would put a
        // second reader on vars `every_resource_env_field_drives_its_resolve_path` sets and clears
        // process-wide, which is the race that test's comment says it is safe from.
        for entry in RESOURCES {
            let path = entry.default_path;
            let text = std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("read the {} at {path}: {e}", entry.noun));
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
        // The gist names verbs and is handed to every model that connects, so it is prose with
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
        // The tool answers "what is playing?" with the engine's own projection of the source it
        // installed, plus the source and hash to reconcile against — no document reaches this door.
        let snapshot = DocumentSnapshot {
            content_hash: "00c0ffee".to_string(),
            source: Some("voices/warm.json".to_string()),
        };
        let fake = FakeTransport::reachable()
            .with_document(snapshot)
            .with_call(
                names::DESCRIBE_INSTRUMENT,
                Response::Called {
                    output: serde_json::json!({
                        "view": "index",
                        "text": "instrument warm · 1 node\n/osc oscillator",
                    }),
                    summary: "index of voices/warm.json".to_string(),
                },
            );
        let requests = fake.request_log();
        let result = block_on(server_with(fake).get_current_instrument()).expect("result");
        assert_ne!(result.is_error, Some(true));
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(s["content_hash"], serde_json::json!("00c0ffee"));
        assert_eq!(s["source"], serde_json::json!("voices/warm.json"));
        assert_eq!(
            s["projection"],
            serde_json::json!("instrument warm · 1 node\n/osc oscillator"),
            "the projection is the engine's index of the installed source: {s}"
        );
        assert_eq!(
            s.get("document"),
            None,
            "the installed document must never ride back to the model"
        );
        assert!(
            requests
                .lock()
                .expect("request log")
                .contains(&Request::Call {
                    tool: names::DESCRIBE_INSTRUMENT.to_string(),
                    args: serde_json::json!({ "source": "voices/warm.json", "view": "index" }),
                }),
            "the projection is asked of the engine, by the installed source"
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
    fn get_engine_meters_returns_a_level_per_channel_and_summarizes_them() {
        let report = MetersReport {
            outputs: vec![
                ChannelLevel {
                    peak_dbfs: -0.5,
                    rms_dbfs: -3.5,
                },
                ChannelLevel {
                    peak_dbfs: -120.0,
                    rms_dbfs: -120.0,
                },
            ],
            inputs: vec![],
            frames: 48_000,
            probes: Vec::new(),
        };
        let result = block_on(
            server_with(FakeTransport::reachable().with_meters(report)).get_engine_meters(),
        )
        .expect("result");
        assert_ne!(result.is_error, Some(true));
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(s["outputs"][0]["peak_dbfs"], serde_json::json!(-0.5));
        assert_eq!(s["outputs"][1]["rms_dbfs"], serde_json::json!(-120.0));
        assert_eq!(s["inputs"], serde_json::json!([]));
        assert_eq!(s["frames"], serde_json::json!(48_000));
        let text = first_text(&result);
        assert!(
            text.contains("outputs: -0.5/-3.5, -120.0/-120.0; inputs: none"),
            "{text}"
        );
    }

    #[test]
    fn get_engine_meters_advertises_the_same_schema() {
        let schema = advertised_output_schema("get_engine_meters");
        let text = schema.to_string();
        assert!(
            !text.contains("probes") && !text.contains("PortLevel"),
            "the tool is device-only: {text}"
        );
        let mut properties: Vec<_> = schema["properties"]
            .as_object()
            .expect("properties")
            .keys()
            .cloned()
            .collect();
        properties.sort();
        assert_eq!(properties, ["frames", "inputs", "outputs"]);
        let mut defs: Vec<_> = schema["$defs"]
            .as_object()
            .expect("defs")
            .keys()
            .cloned()
            .collect();
        defs.sort();
        assert_eq!(defs, ["ChannelLevel"]);
    }

    #[test]
    fn get_engine_meters_never_serializes_node_levels() {
        let report = MetersReport {
            probes: vec![engine::ProbeLevels {
                path: vec!["osc".to_string()],
                ports: Vec::new(),
            }],
            ..MetersReport::default()
        };
        let result = block_on(
            server_with(FakeTransport::reachable().with_meters(report)).get_engine_meters(),
        )
        .expect("result");
        let s = result.structured_content.as_ref().expect("structured");
        assert!(s.get("probes").is_none(), "{s}");
    }

    #[test]
    fn get_engine_meters_unreachable_is_iserror() {
        let result = block_on(server_with(FakeTransport::unreachable()).get_engine_meters())
            .expect("result");
        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("reuben play"));
    }

    #[test]
    fn no_advertised_schema_carries_an_instrument_document() {
        // The no-document-in-context claim as a build-time property rather than a cleanup that
        // happened once. It bites on the field NAME, walked over every input and output schema
        // including `$defs`, because that is how the retired arms spelled it and how a
        // re-introduction would spell it too.
        fn walk(node: &serde_json::Value, tool: &str, whose: &str, path: &str) {
            match node {
                serde_json::Value::Object(map) => {
                    if let Some(serde_json::Value::Object(props)) = map.get("properties") {
                        for name in props.keys() {
                            assert_ne!(
                                name, "document",
                                "{tool}'s {whose} schema carries a `document` field at {path} — no \
                                 arm moves instrument JSON; name the document by `source` and \
                                 answer with a projection"
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

    fn open_pad(expect: Option<&str>) -> engine::OpenInstrument {
        engine::OpenInstrument {
            name: "pad.json".to_string(),
            expect: expect.map(str::to_string),
        }
    }

    #[test]
    fn open_instrument_expect_mismatch_returns_conflict_no_install() {
        // An expect-guard miss is an ordinary result (the guard guarding, not the tool failing):
        // nothing is installed, and the conflict names both hashes so the model reconciles.
        let fake = FakeTransport::reachable().with_open(Response::Conflict(Conflict {
            expected: "0badc0de".to_string(),
            actual: "00c0ffee".to_string(),
        }));
        let result =
            block_on(server_with(fake).open_instrument(Parameters(open_pad(Some("0badc0de")))))
                .expect("open returns a result");
        assert_ne!(
            result.is_error,
            Some(true),
            "a conflict is a reconcilable outcome, not isError: {result:?}"
        );
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(s["ok"], serde_json::json!(false), "nothing installed: {s}");
        assert!(
            s.as_object().is_some_and(|o| !o.contains_key("diff")),
            "a rejected open has no diff to report: {s}"
        );
        assert_eq!(s["conflict"]["expected"], serde_json::json!("0badc0de"));
        assert_eq!(s["conflict"]["actual"], serde_json::json!("00c0ffee"));
        // content_hash names what keeps playing, never the `expected` the client asked for, or the
        // model's next guarded open conflicts again.
        assert_eq!(
            s["content_hash"],
            serde_json::json!("00c0ffee"),
            "a rejected open reports the hash still playing: {s}"
        );
    }

    #[test]
    fn open_instrument_held_reads_as_not_yet_installed_rather_than_failed() {
        let fake = FakeTransport::reachable().with_open(Response::Held(HeldSwap {
            requested: "instruments/groovebox.json".to_string(),
            playing: Some("instruments/acid-techno.json".to_string()),
            content_hash: "00c0ffee".to_string(),
        }));
        let result = block_on(server_with(fake).open_instrument(Parameters(open_pad(None))))
            .expect("open returns a result");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        let s = result
            .structured_content
            .as_ref()
            .expect("structured payload");
        assert_eq!(s["ok"], serde_json::json!(false), "nothing installed: {s}");
        assert_eq!(s["content_hash"], serde_json::json!("00c0ffee"));
        assert_eq!(
            s["held"]["requested"],
            serde_json::json!("instruments/groovebox.json")
        );
        assert_eq!(
            s["held"]["playing"],
            serde_json::json!("instruments/acid-techno.json")
        );
        let text = first_text(&result);
        for expected in [
            "not installed yet",
            "instruments/acid-techno.json keeps playing",
            "get_current_instrument",
            "retrying",
        ] {
            assert!(text.contains(expected), "{expected:?} missing from {text}");
        }
    }

    /// The arguments `set_instrument_name` carries in the tests below.
    fn rename_args() -> authoring::SetInstrumentName {
        authoring::SetInstrumentName {
            source: "pad.json".to_string(),
            name: "warm pad".to_string(),
            expect: Some("00c0ffee".to_string()),
        }
    }

    /// A document verb puts exactly one line on the wire, a `call` naming the verb with the tool's
    /// own arguments, and relays the server's output untouched.
    #[test]
    fn document_verb_is_one_call_to_the_server() {
        let output = serde_json::json!({
            "written": true,
            "hash": "0ddba11",
            "report": { "ok": true, "errors": [], "warnings": [] },
            "zoom": "instrument warm pad",
        });
        let fake = FakeTransport::reachable().with_call(
            names::SET_INSTRUMENT_NAME,
            Response::Called {
                output: output.clone(),
                summary: "renamed to warm pad".to_string(),
            },
        );
        let requests = fake.request_log();
        let result = block_on(server_with(fake).set_instrument_name(Parameters(rename_args())))
            .expect("returns a result");

        assert_eq!(
            *requests.lock().expect("request log"),
            vec![Request::Call {
                tool: names::SET_INSTRUMENT_NAME.to_string(),
                args: serde_json::to_value(rename_args()).expect("args as JSON"),
            }]
        );
        assert_ne!(result.is_error, Some(true), "{result:?}");
        assert_eq!(result.structured_content, Some(output));
        assert_eq!(first_text(&result), "renamed to warm pad");
    }

    /// An edit the server refused to write is still the tool working: an ordinary result whose
    /// `written: false` the model acts on, exactly as when the edit ran in-process.
    #[test]
    fn document_verb_not_written_is_an_ordinary_result() {
        let output = serde_json::json!({
            "written": false,
            "hash": "00c0ffee",
            "report": { "ok": false, "errors": [], "warnings": [] },
        });
        let fake = FakeTransport::reachable().with_call(
            names::SET_INSTRUMENT_NAME,
            Response::Called {
                output: output.clone(),
                summary: "not written".to_string(),
            },
        );
        let result = block_on(server_with(fake).set_instrument_name(Parameters(rename_args())))
            .expect("returns a result");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        assert_eq!(result.structured_content, Some(output));
    }

    #[test]
    fn document_verb_refused_by_the_server_is_iserror_with_its_message() {
        let message = "`pad` does not end in .json; nothing was written";
        let fake = FakeTransport::reachable().with_call(
            names::SET_INSTRUMENT_NAME,
            Response::Refused {
                message: message.to_string(),
            },
        );
        let result = block_on(server_with(fake).set_instrument_name(Parameters(rename_args())))
            .expect("returns a result");
        assert_eq!(result.is_error, Some(true), "{result:?}");
        assert_eq!(result.structured_content, None);
        assert_eq!(first_text(&result), message);
    }

    /// With no engine there is no one to read or write the file, and the door must not fall back
    /// to doing it itself: every document tool is isError, naming the fix.
    #[test]
    fn document_verb_with_no_engine_is_iserror_with_guidance() {
        let server = server_with(FakeTransport::unreachable());
        let results = [
            block_on(server.set_instrument_name(Parameters(rename_args()))),
            block_on(
                server.describe_instrument(Parameters(
                    serde_json::from_value(serde_json::json!({ "source": "pad.json" }))
                        .expect("describe args"),
                )),
            ),
            block_on(
                server.validate_instrument(Parameters(authoring::ValidateInstrument {
                    source: "pad.json".to_string(),
                })),
            ),
        ];
        for result in results {
            let result = result.expect("returns a result");
            assert_eq!(result.is_error, Some(true), "{result:?}");
            assert!(
                first_text(&result).contains(ENGINE_UNREACHABLE_GUIDANCE),
                "the refusal carries the start-the-engine guidance: {result:?}"
            );
        }
    }

    #[test]
    fn describe_operators_answers_with_no_engine() {
        let fake = FakeTransport::unreachable();
        let requests = fake.request_log();
        let result = block_on(server_with(fake).describe_operators(Parameters(
            authoring::DescribeOperators {
                name: Some("oscillator".to_string()),
                compact: true,
            },
        )))
        .expect("returns a result");
        assert_ne!(result.is_error, Some(true), "{result:?}");
        assert!(result.structured_content.is_some(), "{result:?}");
        assert_eq!(
            *requests.lock().expect("request log"),
            Vec::<Request>::new(),
            "listing operators touches no engine"
        );
    }
}
