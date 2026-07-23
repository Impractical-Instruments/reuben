//! The **structural projection** — the agent's entire view of an instrument document.
//!
//! The sibling of [`introspect`](crate::introspect): that module projects the *operator set* and a
//! nested document's *boundary*; this one projects **one document's structure**, because the agent
//! never reads instrument JSON (see rules: agent-mcp). Every door cuts its views from the contract
//! types here, so what the CLI, the MCP sidecar and the web in-page layer show an agent cannot
//! drift — cross-door drift is a compile error, not a runtime surprise
//! (`#portable-tool-contracts`).
//!
//! Four views, **lossless in aggregate**: every field of the document format is reachable through
//! *some* view (index ∪ node-zoom ∪ pipe-view ∪ resources-view), while any single view is
//! deliberately partial — the win is the agent *choosing* a view, not the projection *discarding*
//! data. [`FIELD_COVERAGE`] is where that promise is written down, and the completeness guard walks
//! the real format types against it so a new format field with no view **fails the build**: the
//! failure mode this surface has to defend against is not "too big", it is "the agent never noticed
//! what was missing", and silent omission is exactly what a hand-maintained read surface leaks.
//!
//! - [`Projector::index`] — one line per node (`address type`, plus a dark-resource marker), under
//!   a header carrying the instrument's role line. The map.
//! - [`Projector::zoom`] — one node or a selection: its inputs (literal values **and** wire
//!   sources), its `config` constants, its `doc`, its resource refs, a nested child's boundary, and
//!   **its consumers** — the reverse edges, without which the agent is blind to the blast radius of
//!   every destructive verb. Anything the node ought to show and cannot gets a `note` saying why;
//!   nothing goes missing quietly.
//! - [`Projector::pipes`] — the `interface` boundary: each pipe's type/range/curve/unit/channel.
//! - [`Projector::resources`] — the `id → source` table, who references each id, and whether it
//!   resolved.
//!
//! Every view is a pure function of the document (plus the registry and the resolver), and none of
//! them judges the document: `validate` remains the single authority on whether it loads
//! (`#loader-single-authority`). A document that fails to load still projects — going blind exactly
//! when the agent needs to see is the worst possible failure — with `loadable: false` in the header
//! saying so.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::format::{
    load_instrument_doc, parse_wire, ConfigValue, InputValue, InterfaceEntry, LoadWarning, NodeDoc,
    NormalizedDoc, PipeDefault,
};
use crate::introspect::{describe_patch, first_sentence, PatchBoundary};
use crate::registry::Registry;
use crate::resources::{ResolveError, ResourceResolver};

/// The projection's own shape version, **distinct from the document's `format_version`**: a
/// document can stay v3 while the surface an agent reads is re-cut. Stamped on every header so a
/// wire consumer — and specifically the agent-cost trend, whose numbers *are* projection bytes —
/// can mark a re-baseline instead of silently comparing two different surfaces. Bumped only on a
/// breaking shape change; a new optional line is additive.
pub const PROJECTION_VERSION: u32 = 1;

/// The address that zooms the **document itself** rather than a node: its full `doc`, its name, and
/// its versions. Node addresses are routing prefixes with a segment after the slash, so the bare
/// root cannot collide with a real one in practice — and if a document ever does address a node
/// `/`, the selection simply multi-matches (the grammar already allows that).
pub const DOC_ADDRESS: &str = "/";

/// The one-line notation key for the projection, the sibling of
/// [`COMPACT_DESCRIBE_LEGEND`](crate::introspect::COMPACT_DESCRIBE_LEGEND). A door that ships a
/// projection as standalone grounding prepends this so the notation is self-describing.
pub const PROJECTION_LEGEND: &str =
    "Document projection. Index: one `address type` line per node, \
`dark:<slot>` marks an unresolved resource. Zoom: `in:` bindings (`name<-/source` wired, \
`name=value` literal), `config:` plan-time constants, `out:` consumers as `port->/node.input` or \
`port->pipe:name` (the blast radius of removing the node), `res:` its resource refs, `boundary:` a \
nested child's pipes, `note:` something expected but absent, and why. Pipes: \
`name:type unit exp lo..hi=default chN` for an input pipe, `name<-/node.port chN` for an output \
pipe. Zoom `/` for the document's full doc text. A name carrying whitespace or a separator is \
\"quoted\"; a view that can say nothing says why, never nothing.";

/// What a view says when the document did not load: every resolved/dark judgement below it was
/// never made. `validate` is the authority on *why*; this only stops the views that report
/// resolution state from presenting "unchecked" as "fine".
const LOAD_CAVEAT: &str = "DOCUMENT DOES NOT LOAD — resource resolution unchecked; run validate";

/// One document-controlled string, rendered so it cannot forge structure.
///
/// The projection is a **grammar an agent parses** — `address type`, `name<-source`,
/// `port->/node.input`, comma-separated lists — and nothing validates a node address, an
/// instrument name, a pipe name or a resource id against that grammar: the mint gates the
/// document's *version and shape*, not its spelling. A node addressed `"in: freq=999"` loads
/// cleanly through the real engine path and would otherwise emit a line indistinguishable from a
/// real input binding on the node above it, and a newline anywhere forges an entire second record.
///
/// So: bare when the token is unambiguous, Rust-debug-quoted (which escapes newlines, tabs and
/// quotes) the moment it contains whitespace or any separator the grammar spends. Ordinary
/// documents are untouched, so this costs nothing on the size budget — it only ever fires on a
/// spelling that would have lied.
fn token(s: &str) -> String {
    let ambiguous = s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || matches!(c, ',' | ':' | '<' | '>' | '='));
    if ambiguous {
        format!("{s:?}")
    } else {
        s.to_string()
    }
}

/// Free prose that came from **somewhere else** — a loader warning, a child document's own load
/// error — rendered so it cannot forge structure either.
///
/// [`token`] is wrong for these: they are sentences, so quoting-when-ambiguous would quote all of
/// them anyway, and they are not names to be matched. What matters is that they are *contained*:
/// whitespace collapses (a record is a line, so an embedded newline is the forgery), and the whole
/// thing is quoted, so a `, ` inside a loader message cannot forge an entry in a comma-joined list.
/// These strings carry document text transitively — a resolver error embeds the `resources` source
/// it failed on, a describe error embeds a child document's own node addresses — so "it's only an
/// error message" is exactly backwards.
fn message(s: &str) -> String {
    format!("{:?}", s.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// Which members a view is cut for — one grammar shared by node zoom and the pipe view, because
/// the verbs are both address-shaped and type-shaped (a nudge targets operator types; a
/// blast-radius read targets one address).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    /// Every member.
    All,
    /// Explicit names: node addresses for [`zoom`](Projector::zoom), pipe names for
    /// [`pipes`](Projector::pipes). A name that matches nothing is reported in the view's
    /// `unmatched` list rather than silently dropped.
    Names(Vec<String>),
    /// A type predicate: the operator type for [`zoom`](Projector::zoom), the declared pipe type
    /// for [`pipes`](Projector::pipes) (which therefore matches input pipes only — an output pipe
    /// inherits its type from the port feeding it and declares none).
    Type(String),
}

impl Selection {
    /// A [`Names`](Self::Names) selection from anything string-ish.
    pub fn names<I: IntoIterator<Item = S>, S: Into<String>>(names: I) -> Self {
        Selection::Names(names.into_iter().map(Into::into).collect())
    }
}

/// A literal value in a document: a number or a vocab-enum/`Symbol` name. The one shape
/// [`InputValue`], [`ConfigValue`] and [`PipeDefault`] all collapse to for reading.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum Scalar {
    Number(f64),
    Symbol(String),
}

impl Scalar {
    /// Shortest faithful rendering: `f64`'s `Display` is the shortest round-trip decimal (`4`,
    /// `0.72` — never `4.0`), and a symbol renders bare, exactly as
    /// [`signature_fragment`](crate::introspect::PortInfo::signature_fragment) renders an enum
    /// default.
    fn render(&self) -> String {
        match self {
            Scalar::Number(n) => n.to_string(),
            Scalar::Symbol(s) => token(s),
        }
    }
}

/// Everything true of the **document** rather than of any node: its identity, its two versions, its
/// size, and its `doc` — the authorial intent that explains every pipe below it.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DocHeader {
    pub instrument: String,
    pub format_version: u32,
    pub projection_version: u32,
    /// Node count — the document's size at a glance, so the agent knows what an index costs before
    /// asking for one.
    pub nodes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Whether the document loads through the real engine path. `false` does **not** make the
    /// projection wrong — it means the dark-resource markers are unknown, and that `validate` (the
    /// single authority) has something to say.
    pub loadable: bool,
}

impl DocHeader {
    fn line(&self) -> String {
        let mut s = format!(
            "instrument {} ({} nodes, format {}, projection {}",
            token(&self.instrument),
            self.nodes,
            self.format_version,
            self.projection_version
        );
        if !self.loadable {
            s.push_str(", DOES NOT LOAD — run validate");
        }
        s.push(')');
        s
    }

    /// The header as an index prefixes it: the identity line plus the **role line** — the `doc`'s
    /// first sentence, the same projection the library index takes. Intent stays *reachable*, not
    /// *resident*: when the doc says more, the line says how much more and where to get it.
    fn render_role(&self) -> String {
        let mut out = self.line();
        // A `doc` of "" or "   " is not intent, and a bare `doc:` line is a wasted line on the
        // highest-frequency read.
        if let Some(whole) = self.one_line_doc() {
            let role = first_sentence(&whole);
            // Characters, not bytes: the agent spends this number deciding whether the rest of the
            // doc is worth a turn, and this codebase's prose is full of em-dashes and arrows.
            let rest = whole.chars().count() - role.chars().count();
            out.push_str(&format!("\ndoc: {role}"));
            if rest > 0 {
                out.push_str(&format!(" [+{rest} chars: zoom {DOC_ADDRESS}]"));
            }
        }
        out
    }

    /// The header as the document zoom renders it: the identity line plus the **whole** `doc`,
    /// whitespace-normalized onto one line.
    fn render_full(&self) -> String {
        let mut out = self.line();
        if let Some(whole) = self.one_line_doc() {
            out.push_str(&format!("\ndoc: {whole}"));
        }
        out
    }

    /// The `doc` whitespace-normalized onto one line — `None` when there is nothing to say. A
    /// document's `doc` is the one place the projection deliberately carries prose rather than a
    /// token, so normalizing it here is what keeps it from spanning records.
    fn one_line_doc(&self) -> Option<String> {
        self.doc
            .as_deref()
            .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
            .filter(|d| !d.is_empty())
    }
}

/// One node's line in the index: what it is, and the single marker the index carries.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct IndexEntry {
    pub address: String,
    #[serde(rename = "type")]
    pub type_name: String,
    /// The resource slot (`sample`/`voice`/`patch`) whose reference did not resolve this load —
    /// the one decision-state marker the index carries. Reachability and silence stay `validate`
    /// warnings; the marker set is deliberately small and reviewed, because every marker is a
    /// per-node cost on the highest-frequency read.
    ///
    /// On a document that does not load, only the document-level half is knowable (an id with no
    /// row in the `resources` table), so a marker may be **absent** where a load would have found
    /// one. The index does not repeat the zoom's load caveat for that — a marker it *does* show is
    /// never false, and the header's `DOES NOT LOAD` is already on the line above. That is a
    /// byte-budget choice on the most-read view, not an oversight.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dark: Option<String>,
}

/// The **node index**: the agent's map of a document.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct NodeIndex {
    pub header: DocHeader,
    pub nodes: Vec<IndexEntry>,
}

impl NodeIndex {
    /// The index as an agent reads it: header, role line, then one `address type` line per node.
    pub fn render(&self) -> String {
        let mut out = self.header.render_role();
        for n in &self.nodes {
            out.push_str(&format!("\n{} {}", token(&n.address), token(&n.type_name)));
            if let Some(slot) = &n.dark {
                out.push_str(&format!(" dark:{slot}"));
            }
        }
        out
    }
}

/// One of a node's inputs: the name plus what is bound to it — a wire, or a literal.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct InputEdge {
    pub name: String,
    /// The wire-ref feeding this input, verbatim as the document spells it (`"/clock.gate"`, or
    /// `"/kick"` under the sole-output sugar).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The literal bound to this input, when it is not wired.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<Scalar>,
}

impl InputEdge {
    fn render(&self) -> String {
        match (&self.source, &self.value) {
            (Some(src), _) => format!("{}<-{}", token(&self.name), token(src)),
            (None, Some(v)) => format!("{}={}", token(&self.name), v.render()),
            // Unreachable through `InputValue` (a binding is a wire or a literal), rendered rather
            // than panicked so a future third form degrades visibly instead of vanishing.
            (None, None) => format!("{}=?", token(&self.name)),
        }
    }
}

/// What one of a node's outputs feeds — the reverse edge. Mandatory, not a nicety: consumers are
/// otherwise derivable only by scanning every node, which the index deliberately does not carry,
/// so without this the agent cannot see what a `remove_node` would break.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OutEdge {
    /// The output port this edge leaves from. `None` only when the consumer used the sole-output
    /// sugar and the port could be resolved from neither the source's descriptor nor, for a nested
    /// node whose ports live on its child's face, that boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    /// The consuming node's address — absent when the consumer is an interface output pipe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The consuming node's input name, or the output pipe's name.
    pub input: String,
}

impl OutEdge {
    fn render(&self) -> String {
        let port = self
            .port
            .as_deref()
            .map(token)
            .unwrap_or_else(|| "?".to_string());
        match &self.node {
            Some(node) => format!("{port}->{}.{}", token(node), token(&self.input)),
            None => format!("{port}->pipe:{}", token(&self.input)),
        }
    }
}

/// A node's resource reference, and whether it resolved this load.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ResourceRef {
    /// The descriptor slot the reference targets: `"sample"`, `"voice"` or `"patch"`.
    pub slot: String,
    pub id: String,
    /// The node making the reference — carried where the reference is listed away from its node
    /// (the resources view's dangling list). A dangling id is precisely the one an agent has to go
    /// fix, so "which node?" must be a **field**, not a sentence it has to parse out of `dark`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    /// The `resources` table's source for this id — `None` when the id is not in the table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Why the reference is dark, when it is. `None` is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dark: Option<String>,
}

impl ResourceRef {
    fn render(&self) -> String {
        let mut s = format!("{}={}", self.slot, token(&self.id));
        if let Some(node) = &self.node {
            s.push_str(&format!(" on {}", token(node)));
        }
        if let Some(src) = &self.source {
            s.push_str(&format!(" -> {}", token(src)));
        }
        if let Some(why) = &self.dark {
            s.push_str(&format!(" DARK: {}", message(why)));
        }
        s
    }
}

/// One node, zoomed.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct NodeZoom {
    pub address: String,
    #[serde(rename = "type")]
    pub type_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Plan-time `Constant`s — a **different verb** from inputs (they are set in the node's
    /// `config` block and never wired), so they render as their own line.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub config: Vec<InputEdge>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<InputEdge>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub consumers: Vec<OutEdge>,
    /// **Every** resource reference the node carries, not just the first — the index's dark marker
    /// scans all three slots, so a zoom showing one could contradict it.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceRef>,
    /// A nested child's face — **never its internals**. The format is not recursive (a child is a
    /// resource id), so the projection mirrors the *document*: an opaque node plus the boundary it
    /// presents. The child's own nodes are reached by projecting the child document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary: Option<PatchBoundary>,
    /// Why something the agent would expect on this node is **absent** — today, only a nested
    /// child whose reference is fine but whose document will not resolve or describe. Dropping
    /// that silently would be exactly the failure this surface exists to prevent, and a dark `res:`
    /// line does *not* cover it: the reference resolved, the document did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl NodeZoom {
    /// One node's block: the header line, then only the lines it has anything to say on.
    pub fn render(&self) -> String {
        let mut out = format!("{} {}", token(&self.address), token(&self.type_name));
        let mut line = |label: &str, body: String| {
            if !body.is_empty() {
                out.push_str(&format!("\n{label}: {body}"));
            }
        };
        if let Some(doc) = &self.doc {
            line("doc", doc.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        line("config", render_list(&self.config, InputEdge::render));
        line("in", render_list(&self.inputs, InputEdge::render));
        line("out", render_list(&self.consumers, OutEdge::render));
        line("res", render_list(&self.resources, ResourceRef::render));
        if let Some(b) = &self.boundary {
            line("boundary", render_boundary(b));
        }
        if let Some(n) = &self.note {
            line("note", n.clone());
        }
        out
    }
}

/// A **node zoom**: the selected nodes, plus the document header when the selection asked for it.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct Zoom {
    /// Present iff the selection named [`DOC_ADDRESS`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header: Option<DocHeader>,
    pub nodes: Vec<NodeZoom>,
    /// How many nodes there were to select from, so "nothing matched" can be told apart from
    /// "there is nothing here".
    pub declared: usize,
    /// Whether the document loads. Carried on the view, not only on the optional header: a zoom's
    /// `res:` lines report resolution state, and when the load never completed that state is
    /// **unknown** rather than clean.
    pub loadable: bool,
    /// Selection terms that matched nothing. Reported, never silent — "no such node" is a fact the
    /// agent's next verb depends on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched: Vec<String>,
}

impl Zoom {
    pub fn render(&self) -> String {
        // The ANSWER first, then the caveat is prepended. The caveat is not an answer, so it must
        // not count as one: pushing it first would make `blocks` non-empty and silently swallow
        // the "there are no nodes" sentence on exactly the documents that need it most.
        let mut blocks: Vec<String> = Vec::new();
        if let Some(h) = &self.header {
            blocks.push(h.render_full());
        }
        blocks.extend(self.nodes.iter().map(NodeZoom::render));
        if !self.unmatched.is_empty() {
            blocks.push(format!(
                "no match: {}",
                render_list(&self.unmatched, |t| token(t))
            ));
        }
        // Never the empty string — it is indistinguishable from a truncated or failed call — but
        // only when nothing else was said: a `/` zoom asked for the document, not for nodes, and
        // "0 of 53 shown" would read as a failure rather than an answer.
        if blocks.is_empty() {
            blocks.push(match self.declared {
                0 => "nodes (0): this document has no nodes".to_string(),
                n => format!("nodes (0 of {n} shown)"),
            });
        }
        // Unconditional on `loadable`, NOT on the header's absence. The header's own
        // `DOES NOT LOAD` marker says the document is broken; it does not say that every `res:`
        // line below it went unchecked, and those are different facts. Making the caveat depend on
        // the header meant that merely adding `/` to a selection — "show me the doc and this node"
        // — silently downgraded what the agent was told about the node.
        if !self.loadable {
            blocks.insert(0, LOAD_CAVEAT.to_string());
        }
        blocks.join("\n")
    }
}

/// One `interface` pipe. Both directions share this shape because [`InterfaceEntry`] is one
/// untagged union: serde will put either pipe form in either map, and while the mint rejects a
/// misfiled entry today, a reader that hard-assumes direction is one gate change away from lying
/// about a document rather than merely omitting from it.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PipeInfo {
    pub name: String,
    /// The declared `Arg` type — input pipes only; an output pipe inherits the type of the port
    /// feeding it.
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    pub ty: Option<String>,
    /// The internal wire-ref feeding an output pipe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Scalar>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// `"lin"` or `"exp"` — the sweep hint a scripted nudge reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curve: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// An un-migrated v1 target-pointing entry. Unreachable post-mint (migration rewrites the v1
    /// forms, and the loader rejects what it cannot rewrite) — carried so that if one ever does
    /// survive, it is *visible* rather than projected as an empty pipe.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub v1_target: Option<String>,
}

impl PipeInfo {
    /// The same fragment grammar the compact operator describe uses —
    /// `name:type unit exp lo..hi=default` — plus `chN` for a channel binding, so one notation
    /// covers ports and pipes.
    fn render(&self) -> String {
        let mut s = token(&self.name);
        if let Some(ty) = &self.ty {
            s.push_str(&format!(":{}", token(ty)));
        }
        if let Some(from) = &self.from {
            s.push_str(&format!("<-{}", token(from)));
        }
        if let Some(t) = &self.v1_target {
            s.push_str(&format!(" v1-target:{}", token(t)));
        }
        if let Some(u) = &self.unit {
            s.push_str(&format!(" {}", token(u)));
        }
        if self.curve.as_deref() == Some("exp") {
            s.push_str(" exp");
        }
        match (self.min, self.max) {
            (Some(min), Some(max)) => s.push_str(&format!(" {min}..{max}")),
            (Some(min), None) => s.push_str(&format!(" {min}..")),
            (None, Some(max)) => s.push_str(&format!(" ..{max}")),
            (None, None) => {}
        }
        if let Some(d) = &self.default {
            s.push_str(&format!("={}", d.render()));
        }
        if let Some(c) = self.channel {
            s.push_str(&format!(" ch{c}"));
        }
        s
    }
}

/// The **pipe view**: the document's `interface` boundary, the highest-frequency authoring surface
/// in a real instrument (90 pipes and a sixth of the bytes in `acid-techno.json`), which is why it
/// takes a [`Selection`] rather than only ever dumping flat.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PipeView {
    pub inputs: Vec<PipeInfo>,
    pub outputs: Vec<PipeInfo>,
    /// How many pipes the document declares in total, so "the selection matched nothing" can be
    /// told apart from "this document has no interface" — two very different next moves.
    pub declared: usize,
    /// Selection terms that matched no pipe in either map.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched: Vec<String>,
}

impl PipeView {
    pub fn render(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        let shown = self.inputs.len() + self.outputs.len();
        // A section header is a SHOWN count. Under a selection that says nothing about how many
        // pipes the document has, and `pipes in (1):` on a 91-pipe instrument reads as the total —
        // so when the view is a subset, it says so before the subset.
        if shown < self.declared {
            lines.push(format!("pipes ({shown} of {} shown):", self.declared));
        }
        for (label, pipes) in [("in", &self.inputs), ("out", &self.outputs)] {
            if pipes.is_empty() {
                continue;
            }
            lines.push(format!("pipes {label} ({}):", pipes.len()));
            lines.extend(pipes.iter().map(PipeInfo::render));
        }
        if !self.unmatched.is_empty() {
            lines.push(format!(
                "no match: {}",
                render_list(&self.unmatched, |t| token(t))
            ));
        }
        // A view must always answer with a *sentence*: an empty string is indistinguishable from a
        // truncated or failed call. Only one case can still reach here — a document with no
        // `interface` at all, where there was no subset line to draw either — and it is a
        // different answer from "your selection matched none of them", which sends the agent
        // somewhere else entirely.
        if lines.is_empty() {
            lines.push("pipes (0): this document declares no interface".to_string());
        }
        lines.join("\n")
    }
}

/// One node's use of a resource id: which node, through which slot.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ResourceUse {
    pub slot: String,
    pub node: String,
}

impl ResourceUse {
    fn render(&self) -> String {
        format!("{}:{}", self.slot, token(&self.node))
    }
}

/// One `resources` entry: what it points at, who references it, and whether it resolved.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ResourceEntry {
    pub id: String,
    /// The source the id names — a filesystem path natively, a store key on web. **Opaque**: only
    /// the door's resolver interprets it.
    pub source: String,
    /// Who references this id, in document order. Empty means the entry is unreferenced — legal,
    /// and ignored by the loader, but worth seeing. Structured rather than pre-rendered: baking
    /// the text view's quoting into the data model would hand a door an address it has to unescape
    /// and split, while every other address in this surface's serde shape is raw.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<ResourceUse>,
    /// Why the id did not resolve this load, when it did not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dark: Option<String>,
}

/// The **resources view**: the `id → source` table plus the per-node references and resolved/dark
/// state that the index compresses to a single marker.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ResourcesView {
    pub entries: Vec<ResourceEntry>,
    /// References to an id the `resources` table does not carry — a dangling ref has no table row
    /// to hang off, so it gets its own list rather than vanishing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dangling: Vec<ResourceRef>,
    /// Whether the document loads. This is the view whose entire job is resolved/dark state, and
    /// resolution is discovered **by loading**: on a document that does not load, "no DARK" means
    /// *nobody checked*, not *it resolves*. Reporting the first as the second would be the
    /// projection lying rather than omitting.
    pub loadable: bool,
}

impl ResourcesView {
    pub fn render(&self) -> String {
        // The header counts BOTH kinds, and every dangling line is labelled: a count that
        // disagreed with the line count, over rows in two different grammars, is a table an agent
        // cannot parse.
        let mut lines = Vec::new();
        if !self.loadable {
            lines.push(LOAD_CAVEAT.to_string());
        }
        lines.push(if self.dangling.is_empty() {
            format!("resources ({}):", self.entries.len())
        } else {
            format!(
                "resources ({} listed, {} dangling):",
                self.entries.len(),
                self.dangling.len()
            )
        });
        for e in &self.entries {
            let mut s = format!("{} -> {}", token(&e.id), token(&e.source));
            if e.refs.is_empty() {
                s.push_str(" unreferenced");
            } else {
                s.push_str(&format!(" {}", render_list(&e.refs, ResourceUse::render)));
            }
            if let Some(why) = &e.dark {
                s.push_str(&format!(" DARK: {}", message(why)));
            }
            lines.push(s);
        }
        for d in &self.dangling {
            lines.push(format!("dangling {}", d.render()));
        }
        lines.join("\n")
    }
}

/// What only a real load can tell the projection: whether the document builds, and which resource
/// references went dark doing it.
#[derive(Default)]
struct LoadFacts {
    loadable: bool,
    /// Resource ids that did not resolve → why.
    dark_ids: BTreeMap<String, String>,
    /// Node addresses the loader called dark for a reason that is not an id — today only a
    /// `subpatch` carrying no `patch` reference.
    dark_nodes: BTreeMap<String, String>,
}

/// The projection source: one mint, at most one load, from which every view is cut. Doors hold this
/// for a turn so a verb can echo the zoom of what it touched without re-parsing.
///
/// The load is **lazy and memoized**, and so is each nested child's boundary. That is not
/// micro-optimization: a load resolves and decodes every referenced sample, and describing a child
/// mints and loads a whole second document. On the one surface whose entire justification is
/// per-turn cost, a `pipes` call must not pay for a WAV decode, and `zoom --type voicer` on a
/// five-voice rig must not load the same voice document five times over.
pub struct Projector<'a> {
    doc: NormalizedDoc,
    registry: &'a Registry,
    resolver: &'a dyn ResourceResolver,
    /// Filled on the first view that needs a dark marker; `pipes` never touches it.
    load: OnceCell<LoadFacts>,
    /// Canonical child source → its described face (or why it has none).
    boundaries: RefCell<BTreeMap<String, Result<PatchBoundary, String>>>,
    /// `source node address` → its consumers. Built eagerly: inverting the document's own edges is
    /// pure in-memory work with no IO, and every view but `pipes` wants it.
    consumers: BTreeMap<String, Vec<OutEdge>>,
}

impl<'a> Projector<'a> {
    /// Mint the document. The mint is required — without a parseable document there is no structure
    /// to project — but the **load is best-effort and deferred**: a document that fails to load
    /// still projects, with `loadable: false` in the header and no dark-resource markers, because
    /// `validate` is the single authority on validity and going blind is the worst way to report
    /// invalidity.
    pub fn new(
        json: &str,
        registry: &'a Registry,
        resolver: &'a dyn ResourceResolver,
    ) -> Result<Self, String> {
        // The mint is the parse + version gate + migration; its failure is a document with no
        // shape to read, so this is the one thing the projection cannot degrade past.
        let doc =
            NormalizedDoc::from_json(json, registry, Some(resolver)).map_err(|e| e.to_string())?;
        let consumers = build_consumers(&doc, registry);
        Ok(Projector {
            doc,
            registry,
            resolver,
            load: OnceCell::new(),
            boundaries: RefCell::new(BTreeMap::new()),
            consumers,
        })
    }

    /// Load the document once, on the first view that needs to know what went dark.
    fn facts(&self) -> &LoadFacts {
        self.load.get_or_init(|| {
            let Ok(loaded) = load_instrument_doc(&self.doc, self.registry, self.resolver) else {
                return LoadFacts::default();
            };
            let mut facts = LoadFacts {
                loadable: true,
                ..LoadFacts::default()
            };
            for w in &loaded.warnings {
                collect_dark(w, &mut facts.dark_ids, &mut facts.dark_nodes);
            }
            facts
        })
    }

    fn header(&self) -> DocHeader {
        DocHeader {
            instrument: self.doc.instrument.clone(),
            format_version: self.doc.format_version,
            projection_version: PROJECTION_VERSION,
            nodes: self.doc.nodes.len(),
            doc: self.doc.doc.clone(),
            loadable: self.facts().loadable,
        }
    }

    /// The dark marker for one node: the first resource slot whose reference did not resolve, or
    /// the loader's own node-level darkness.
    fn dark_slot(&self, node: &NodeDoc) -> Option<String> {
        for (slot, r) in node.resource_refs() {
            let Some(id) = r else { continue };
            // Either the load said the id went dark, or the document itself has no row for it —
            // the second is true whether or not the document loaded, so the marker survives a
            // `loadable: false` projection.
            if self.facts().dark_ids.contains_key(id) || !self.doc.resources.contains_key(id) {
                return Some(slot.to_string());
            }
        }
        // A `subpatch` carrying no `patch` reference at all: dark with no id to blame it on.
        self.facts()
            .dark_nodes
            .get(&node.address)
            .map(|_| "patch".to_string())
    }

    /// The **node index** — every node, one line each.
    pub fn index(&self) -> NodeIndex {
        NodeIndex {
            header: self.header(),
            nodes: self
                .doc
                .nodes
                .iter()
                .map(|n| IndexEntry {
                    address: n.address.clone(),
                    type_name: n.type_name.clone(),
                    dark: self.dark_slot(n),
                })
                .collect(),
        }
    }

    /// **Zoom** the selected nodes. [`DOC_ADDRESS`] in a [`Selection::Names`] additionally returns
    /// the document header with its full `doc`.
    pub fn zoom(&self, sel: &Selection) -> Zoom {
        let mut matched: BTreeSet<usize> = BTreeSet::new();
        let mut unmatched: Vec<String> = Vec::new();
        let mut header = None;
        match sel {
            Selection::All => matched.extend(0..self.doc.nodes.len()),
            Selection::Names(names) => {
                for name in names {
                    let mut hit = false;
                    if name == DOC_ADDRESS {
                        header = Some(self.header());
                        hit = true;
                    }
                    for (i, n) in self.doc.nodes.iter().enumerate() {
                        if &n.address == name {
                            matched.insert(i);
                            hit = true;
                        }
                    }
                    if !hit {
                        unmatched.push(name.clone());
                    }
                }
            }
            Selection::Type(ty) => {
                for (i, n) in self.doc.nodes.iter().enumerate() {
                    if &n.type_name == ty {
                        matched.insert(i);
                    }
                }
                if matched.is_empty() {
                    unmatched.push(ty.clone());
                }
            }
        }
        Zoom {
            header,
            nodes: matched.iter().map(|&i| self.zoom_node(i)).collect(),
            declared: self.doc.nodes.len(),
            loadable: self.facts().loadable,
            unmatched,
        }
    }

    /// Every resource reference a node carries, in the format's own slot order — **all** of them,
    /// not the first: the index's dark marker scans every slot, so a zoom that showed only one
    /// could tell the agent a node is dark and then show it a healthy resource.
    fn refs_of(&self, n: &NodeDoc) -> Vec<ResourceRef> {
        n.resource_refs()
            .into_iter()
            .filter_map(|(slot, r)| r.as_ref().map(|id| (slot, id)))
            .map(|(slot, id)| {
                let source = self.doc.resources.get(id).cloned();
                let dark = self.facts().dark_ids.get(id).cloned().or_else(|| {
                    source
                        .is_none()
                        .then(|| format!("{id:?} is not in the resources table"))
                });
                ResourceRef {
                    slot: slot.to_string(),
                    id: id.clone(),
                    // Implicit: this ref is being rendered on its own node's zoom.
                    node: None,
                    source,
                    dark,
                }
            })
            .collect()
    }

    /// A nested child's face, memoized per source: `zoom --type voicer` on a five-voice rig would
    /// otherwise resolve, mint and load each child document once per zoom, and again on the next
    /// zoom of the same node. Returns the failure rather than dropping it — see [`NodeZoom::note`].
    fn boundary_of(&self, r: &ResourceRef) -> Result<PatchBoundary, String> {
        let Some(source) = r.source.as_deref() else {
            return Err(format!("{:?} is not in the resources table", r.id));
        };
        let id = self.resolver.canonical(source, None);
        if let Some(hit) = self.boundaries.borrow().get(&id) {
            return hit.clone();
        }
        // Resolved through the same seam the loader uses — and **rebased on the child**, so the
        // child's own nested references resolve next to it exactly as they do under a real load.
        // Without that, a child that itself nests describes every re-exported port as dark, which
        // would be the projection lying rather than omitting.
        let described = self
            .resolver
            .resolve_text(&id)
            .map_err(|e| format!("{source:?} did not resolve: {e}"))
            .and_then(|text| {
                let rebased = Rebased {
                    inner: self.resolver,
                    referrer: id.clone(),
                };
                describe_patch(&text, self.registry, &rebased)
                    .map_err(|e| format!("{source:?} did not describe: {e}"))
            });
        self.boundaries.borrow_mut().insert(id, described.clone());
        described
    }

    fn zoom_node(&self, i: usize) -> NodeZoom {
        let n = &self.doc.nodes[i];
        let resources = self.refs_of(n);
        // At most one nesting reference per node in practice (a Voicer's `voice`, a subpatch's
        // `patch`); the first is the one whose face this node presents.
        let nested = resources.iter().find(|r| r.slot != "sample");
        let (boundary, note) = match nested.map(|r| (r, self.boundary_of(r))) {
            Some((_, Ok(b))) => (Some(b), None),
            // A dark ref already explains itself on the `res:` line — no second complaint. The
            // note is for the case the `res:` line CANNOT cover: the reference resolved, and the
            // document behind it still did not describe.
            Some((r, Err(_))) if r.dark.is_some() => (None, None),
            Some((_, Err(why))) => (None, Some(format!("no boundary: {}", message(&why)))),
            // No reference at all — and the loader may still have called the node dark for it (a
            // `subpatch` carrying no `patch`). There is no `res:` line to hang that on, so without
            // a note the index says `dark:patch` and the zoom the agent goes to next shows a clean
            // node.
            None => (
                None,
                self.facts()
                    .dark_nodes
                    .get(&n.address)
                    .map(|why| message(why)),
            ),
        };
        NodeZoom {
            address: n.address.clone(),
            type_name: n.type_name.clone(),
            doc: n.doc.clone(),
            config: n
                .config
                .iter()
                .map(|(name, v)| InputEdge {
                    name: name.clone(),
                    source: None,
                    value: Some(match v {
                        ConfigValue::Number(x) => Scalar::Number(*x),
                        ConfigValue::Symbol(s) => Scalar::Symbol(s.clone()),
                    }),
                })
                .collect(),
            inputs: n
                .inputs
                .iter()
                .map(|(name, v)| match v {
                    InputValue::Wire { from } => InputEdge {
                        name: name.clone(),
                        source: Some(from.clone()),
                        value: None,
                    },
                    InputValue::Number(x) => InputEdge {
                        name: name.clone(),
                        source: None,
                        value: Some(Scalar::Number(*x)),
                    },
                    InputValue::Symbol(s) => InputEdge {
                        name: name.clone(),
                        source: None,
                        value: Some(Scalar::Symbol(s.clone())),
                    },
                })
                .collect(),
            consumers: {
                let mut edges = self.consumers.get(&n.address).cloned().unwrap_or_default();
                // A `subpatch` declares no outputs of its own — its ports are the child's face —
                // so `build_consumers` cannot resolve sole-output sugar into one from the registry
                // alone. It is resolvable here, where this node's boundary is already in hand, and
                // the answer would otherwise print as `?` on the line directly above the boundary
                // that names it.
                if let Some(b) = &boundary {
                    if let [only] = b.outputs.as_slice() {
                        for e in edges.iter_mut().filter(|e| e.port.is_none()) {
                            e.port = Some(only.name.clone());
                        }
                    }
                }
                edges
            },
            resources,
            boundary,
            note,
        }
    }

    /// The **pipe view** for the selected pipes.
    pub fn pipes(&self, sel: &Selection) -> PipeView {
        let empty = Default::default();
        let iface = self.doc.interface.as_ref().unwrap_or(&empty);
        let cut = |entries: &BTreeMap<String, InterfaceEntry>| -> Vec<PipeInfo> {
            entries
                .iter()
                .map(|(name, e)| pipe_info(name, e))
                .filter(|p| match sel {
                    Selection::All => true,
                    Selection::Names(names) => names.contains(&p.name),
                    Selection::Type(ty) => p.ty.as_deref() == Some(ty.as_str()),
                })
                .collect()
        };
        let inputs = cut(&iface.inputs);
        let outputs = cut(&iface.outputs);
        let unmatched = match sel {
            Selection::All => Vec::new(),
            Selection::Names(names) => names
                .iter()
                .filter(|n| {
                    !iface.inputs.contains_key(n.as_str())
                        && !iface.outputs.contains_key(n.as_str())
                })
                .cloned()
                .collect(),
            Selection::Type(ty) => {
                if inputs.is_empty() && outputs.is_empty() {
                    vec![ty.clone()]
                } else {
                    Vec::new()
                }
            }
        };
        PipeView {
            inputs,
            outputs,
            declared: iface.inputs.len() + iface.outputs.len(),
            unmatched,
        }
    }

    /// The **resources view**.
    pub fn resources(&self) -> ResourcesView {
        let mut refs: BTreeMap<&str, Vec<ResourceUse>> = BTreeMap::new();
        let mut dangling: Vec<ResourceRef> = Vec::new();
        for n in &self.doc.nodes {
            for (slot, r) in n.resource_refs() {
                let Some(id) = r else { continue };
                if self.doc.resources.contains_key(id) {
                    refs.entry(id).or_default().push(ResourceUse {
                        slot: slot.to_string(),
                        node: n.address.clone(),
                    });
                } else {
                    dangling.push(ResourceRef {
                        slot: slot.to_string(),
                        id: id.clone(),
                        node: Some(n.address.clone()),
                        source: None,
                        dark: Some("not in the resources table".to_string()),
                    });
                }
            }
        }
        ResourcesView {
            entries: self
                .doc
                .resources
                .iter()
                .map(|(id, source)| ResourceEntry {
                    id: id.clone(),
                    source: source.clone(),
                    refs: refs.get(id.as_str()).cloned().unwrap_or_default(),
                    dark: self.facts().dark_ids.get(id).cloned(),
                })
                .collect(),
            dangling,
            loadable: self.facts().loadable,
        }
    }
}

/// A resolver rebased on one document — the projection's stand-in for the `referrer` the loader
/// threads through its recursive passes. Describing a child document is a fresh top-level call, so
/// its own references would otherwise canonicalize against the *root*; wrapping the door's resolver
/// this way makes them canonicalize against the child, which is exactly what a real load does.
/// Everything else forwards untouched — the door still owns what a source means.
struct Rebased<'r> {
    inner: &'r dyn ResourceResolver,
    referrer: String,
}

impl ResourceResolver for Rebased<'_> {
    fn resolve(&self, source: &str) -> Result<crate::resources::SampleBuffer, ResolveError> {
        self.inner.resolve(source)
    }

    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        self.inner.resolve_text(source)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.inner.write_text(source, text)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.inner
            .canonical(source, referrer.or(Some(&self.referrer)))
    }
}

/// Invert the document's edges once: every node input and every output pipe's feed, keyed by the
/// **source** node. The sole-output sugar is resolved against the registry so a consumer that
/// wrote `"/kick"` still reports which port it took.
fn build_consumers(doc: &NormalizedDoc, registry: &Registry) -> BTreeMap<String, Vec<OutEdge>> {
    // The addresses a reference can name **whole**: exactly what the loader keys its own
    // exact-match-first on — every node address as written, plus `/{name}` for each input pipe,
    // which is the address the mint puts in the same flat namespace. Nothing forbids a `.` in
    // either (a v1 interface entry named `"my.tone"` migrates to a pipe minting `/my.tone`
    // verbatim), so an exact address resolves BEFORE the last-`.` split. Getting this set wrong in
    // either direction is a lie in the one view the agent trusts to tell it what a removal breaks:
    // too narrow and the edge lands on `/my` instead of `/my.tone`; too broad and it lands on a
    // key no node has, vanishing from the view entirely. Neither is a shape to approximate — the
    // rule is copied from the loader, so it is written the way the loader writes it.
    let addressable: BTreeSet<String> = doc
        .nodes
        .iter()
        .map(|n| n.address.clone())
        .chain(
            doc.interface
                .iter()
                .flat_map(|i| i.inputs.keys().map(|name| format!("/{name}"))),
        )
        .collect();
    let sole_output = |address: &str| -> Option<String> {
        let ty = &doc.nodes.iter().find(|n| n.address == address)?.type_name;
        let desc = &registry.get(ty)?.descriptor;
        match desc.outputs.as_slice() {
            [only] => Some(only.name.to_string()),
            _ => None,
        }
    };
    let mut out: BTreeMap<String, Vec<OutEdge>> = BTreeMap::new();
    let mut record = |reference: &str, node: Option<String>, input: String| {
        let (src, port) = if addressable.contains(reference) {
            (reference, None)
        } else {
            parse_wire(reference)
        };
        let port = port.map(str::to_string).or_else(|| sole_output(src));
        out.entry(src.to_string())
            .or_default()
            .push(OutEdge { port, node, input });
    };
    for n in &doc.nodes {
        for (name, v) in &n.inputs {
            if let InputValue::Wire { from } = v {
                // An input pipe mints `/{pipe}` into the flat node namespace, so some of these
                // land under a key that is a *pipe* rather than a node. Nothing reads those today
                // — the pipe view carries no consumer field — which is the open question on #610.
                record(from, Some(n.address.clone()), name.clone());
            }
        }
    }
    // Both interface maps, matched by **shape** rather than by which map the entry sits in — the
    // same way `pipe_info` reads them. `InterfaceEntry` is one untagged union, so a `Feed`
    // deserializes into `inputs` at the serde layer; the mint rejects that today, so this arm is
    // defensive rather than reachable. It costs two lines and keeps the projection's two readers of
    // the union from ever disagreeing about a document's edges if that gate moves.
    for (name, e) in doc
        .interface
        .iter()
        .flat_map(|i| i.inputs.iter().chain(i.outputs.iter()))
    {
        if let Some(feed) = e.feed() {
            record(&feed.from, None, name.clone());
        }
    }
    out
}

/// Fold one load warning into the dark tables. Only *this* document's own resource failures count:
/// a [`LoadWarning::Nested`] is the child's problem, surfaced when the child is projected, and the
/// remaining warnings are `validate`'s business, not the projection's.
fn collect_dark(
    w: &LoadWarning,
    ids: &mut BTreeMap<String, String>,
    nodes: &mut BTreeMap<String, String>,
) {
    match w {
        LoadWarning::MissingResource { id, .. } | LoadWarning::ResolveFailed { id, .. } => {
            ids.entry(id.clone()).or_insert_with(|| w.to_string());
        }
        LoadWarning::NoPatchRef { node } => {
            nodes.entry(node.clone()).or_insert_with(|| w.to_string());
        }
        _ => {}
    }
}

/// One `interface` entry, projected. Both maps take either variant of the untagged union, so this
/// reads whichever form is actually there rather than assuming direction.
fn pipe_info(name: &str, entry: &InterfaceEntry) -> PipeInfo {
    let mut info = PipeInfo {
        name: name.to_string(),
        ty: None,
        from: None,
        channel: None,
        default: None,
        min: None,
        max: None,
        curve: None,
        unit: None,
        v1_target: None,
    };
    match entry {
        InterfaceEntry::Pipe(p) => {
            info.ty = Some(p.ty.clone());
            info.channel = p.channel;
            info.default = p.default.as_ref().map(|d| match d {
                PipeDefault::Number(n) => Scalar::Number(*n),
                PipeDefault::Symbol(s) => Scalar::Symbol(s.clone()),
            });
            info.min = p.min;
            info.max = p.max;
            info.curve = p.curve.map(|c| match c {
                crate::format::CurveDoc::Lin => "lin".to_string(),
                crate::format::CurveDoc::Exp => "exp".to_string(),
            });
            info.unit = p.unit.clone();
        }
        InterfaceEntry::Feed(f) => {
            info.from = Some(f.from.clone());
            info.channel = f.channel;
            info.min = f.min;
            info.max = f.max;
            info.unit = f.unit.clone();
        }
        InterfaceEntry::Target(t) => info.v1_target = Some(t.clone()),
        InterfaceEntry::Detailed(m) => {
            info.v1_target = Some(m.target.clone());
            info.min = m.min;
            info.max = m.max;
            info.unit = m.unit.clone();
        }
    }
    info
}

fn render_list<T>(items: &[T], f: impl Fn(&T) -> String) -> String {
    items.iter().map(f).collect::<Vec<_>>().join(", ")
}

/// A nested child's face in one fragment — the same compact port grammar `describe` uses, so an
/// agent reads a boundary and an operator signature the same way.
///
/// This is the one line in the projection assembled from a **second document**, so every string it
/// interpolates is that document's to choose: its `instrument` name, and each of its pipe names and
/// units. Those are tokenized *before* the shared fragment grammar sees them — a child pipe named
/// `"gate\nout: audio->/x.y"` would otherwise forge a phantom consumer on the **parent** node.
/// (`signature_fragment` itself is left alone: it is `describe`'s grammar over registry-owned port
/// names, and changing it would move a surface this ticket does not own.)
fn render_boundary(b: &PatchBoundary) -> String {
    let ports = |ps: &[crate::introspect::PortInfo], dark: &[String]| -> String {
        let mut all: Vec<String> = ps
            .iter()
            .map(|p| {
                let mut safe = p.clone();
                safe.name = token(&safe.name);
                if !safe.unit.is_empty() {
                    safe.unit = token(&safe.unit);
                }
                safe.variants = safe.variants.iter().map(|v| token(v)).collect();
                safe.signature_fragment()
            })
            .collect();
        all.extend(dark.iter().map(|d| format!("{}:DARK", token(d))));
        all.join(", ")
    };
    format!(
        "{} ({}) -> {}",
        token(&b.instrument),
        ports(&b.inputs, &b.dark_inputs),
        ports(&b.outputs, &b.dark_outputs)
    )
}

/// The **completeness table**: every leaf field of the instrument document format, and the view it
/// is dispositioned into — a projection view, or an explicit, reasoned `omit:`.
///
/// This is the written form of "lossless in aggregate", and the coverage guard
/// (`tests/projection_coverage.rs`) walks the real format types with schemars and fails the build
/// if enumeration and this table disagree in either direction: a new format field with no view is
/// **undispositioned**, a table row for a field the format no longer has is **stale**. Silent
/// omission — the one failure mode a hand-maintained read surface cannot be trusted on, and the one
/// this surface is permanent enough to be ruined by — becomes a red build.
///
/// The guard proves every field is *dispositioned*; that the code actually emits what a row claims
/// is the golden-projection test's job (`tests/projection_golden.rs`).
pub const FIELD_COVERAGE: &[(&str, &str)] = &[
    // --- the document itself: the header, which every view carries ---
    ("format_version", "doc-header"),
    ("instrument", "doc-header"),
    // Role line in the index header, whole text on a `/` zoom.
    ("doc", "doc-header"),
    ("resources{}", "resources-view"),
    // --- nodes ---
    ("nodes[].type", "index"),
    ("nodes[].address", "index"),
    ("nodes[].doc", "node-zoom"),
    // Plan-time constants: a different verb from inputs, hence its own zoom line.
    ("nodes[].config{}", "node-zoom"),
    // The untagged InputValue splits across two rows: the literal branch...
    ("nodes[].inputs{}", "node-zoom"),
    // ...and the wire branch, which is also what the reverse edges are inverted from.
    ("nodes[].inputs{}.from", "node-zoom"),
    ("nodes[].sample", "node-zoom, resources-view"),
    ("nodes[].voice", "node-zoom, resources-view"),
    ("nodes[].patch", "node-zoom, resources-view"),
    (
        "nodes[].control",
        "omit:retired deserialize-only sink (the mint drains it to a deprecation warning)",
    ),
    // --- v1-only master-tap list: migrated into interface.outputs, illegal in a v2+ document ---
    (
        "outputs[].node",
        "omit:v1-only, migrated into interface.outputs",
    ),
    (
        "outputs[].port",
        "omit:v1-only, migrated into interface.outputs",
    ),
    (
        "outputs[].channel",
        "omit:v1-only, migrated into interface.outputs",
    ),
    // --- interface. InterfaceEntry is ONE untagged union, so both maps enumerate every
    //     variant's fields — and `pipe_info` reads whichever form is actually present. ---
    (
        "interface.inputs{}",
        "omit:v1-only bare Target string form (migrated at the mint; surfaced as v1_target if one \
         ever survives)",
    ),
    ("interface.inputs{}.type", "pipe-view"),
    ("interface.inputs{}.channel", "pipe-view"),
    ("interface.inputs{}.default", "pipe-view"),
    // The range + curve a scripted nudge reads to size its step.
    ("interface.inputs{}.min", "pipe-view"),
    ("interface.inputs{}.max", "pipe-view"),
    ("interface.inputs{}.curve", "pipe-view"),
    ("interface.inputs{}.unit", "pipe-view"),
    ("interface.inputs{}.from", "pipe-view"),
    (
        "interface.inputs{}.target",
        "omit:v1-only migration form (surfaced as v1_target if one ever survives)",
    ),
    (
        "interface.inputs{}.label",
        "omit:retired presentation, lives in a surface doc",
    ),
    (
        "interface.inputs{}.widget",
        "omit:retired presentation, lives in a surface doc",
    ),
    (
        "interface.outputs{}",
        "omit:v1-only bare Target string form (migrated at the mint; surfaced as v1_target if one \
         ever survives)",
    ),
    ("interface.outputs{}.type", "pipe-view"),
    ("interface.outputs{}.channel", "pipe-view"),
    ("interface.outputs{}.default", "pipe-view"),
    ("interface.outputs{}.min", "pipe-view"),
    ("interface.outputs{}.max", "pipe-view"),
    ("interface.outputs{}.curve", "pipe-view"),
    ("interface.outputs{}.unit", "pipe-view"),
    ("interface.outputs{}.from", "pipe-view"),
    (
        "interface.outputs{}.target",
        "omit:v1-only migration form (surfaced as v1_target if one ever survives)",
    ),
    (
        "interface.outputs{}.label",
        "omit:retired presentation, lives in a surface doc",
    ),
    (
        "interface.outputs{}.widget",
        "omit:retired presentation, lives in a surface doc",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resources::{ResolveError, SampleBuffer};

    /// A three-node instrument exercising every zoom line: a literal input, a wire, a pipe-fed
    /// input, a sole-output-sugar consumer, and an output pipe consuming a node port.
    const TINY: &str = r#"{
        "format_version": 3,
        "instrument": "tiny",
        "doc": "A tiny thing. It has a second sentence nobody needs up front.",
        "interface": {
            "inputs": {
                "level": {"type": "f32", "default": 0.5, "min": 0.0, "max": 1.0,
                          "unit": "dB", "curve": "exp"}
            },
            "outputs": { "out": {"from": "/amp", "channel": 0} }
        },
        "nodes": [
            {"type": "oscillator", "address": "/osc", "doc": "the tone",
             "inputs": {"freq": 220.0, "waveform": "Saw"}},
            {"type": "m2s", "address": "/lvl", "inputs": {"in": {"from": "/level"}}},
            {"type": "mul_f32_signal", "address": "/amp",
             "inputs": {"a": {"from": "/osc.audio"}, "b": {"from": "/lvl"}}}
        ]
    }"#;

    /// A voicer whose `voice` resource is absent from the table — the dark path, with no
    /// filesystem in sight.
    const DARK: &str = r#"{
        "format_version": 3,
        "instrument": "dark",
        "resources": { "unused": "nowhere.json" },
        "nodes": [
            {"type": "voicer", "address": "/v", "config": {"voices": 2},
             "voice": "missing-voice"}
        ]
    }"#;

    /// Resolves nothing — every projection here is document-only.
    struct NoResources;

    impl ResourceResolver for NoResources {
        fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(source.to_string()))
        }
    }

    fn projector(json: &str) -> Projector<'static> {
        // Leaked so the fixtures read like the one-liner call sites doors make; a test process
        // owns these for its lifetime anyway.
        let registry: &'static Registry = Box::leak(Box::new(Registry::builtin()));
        let resolver: &'static NoResources = Box::leak(Box::new(NoResources));
        Projector::new(json, registry, resolver).expect("mints")
    }

    #[test]
    fn index_is_one_line_per_node_under_a_role_line() {
        let rendered = projector(TINY).index().render();
        let mut lines = rendered.lines();
        assert_eq!(
            lines.next().unwrap(),
            "instrument tiny (3 nodes, format 3, projection 1)"
        );
        // The role line is the doc's FIRST sentence; the rest is reachable, not resident.
        let doc = lines.next().unwrap();
        assert!(doc.starts_with("doc: A tiny thing."), "{doc}");
        assert!(doc.ends_with("chars: zoom /]"), "{doc}");
        assert!(!doc.contains("second sentence"), "{doc}");
        assert_eq!(
            lines.collect::<Vec<_>>(),
            ["/osc oscillator", "/lvl m2s", "/amp mul_f32_signal"]
        );
    }

    #[test]
    fn zooming_the_document_address_yields_the_whole_doc() {
        let z = projector(TINY).zoom(&Selection::names(["/"]));
        assert!(z.nodes.is_empty());
        assert!(z.unmatched.is_empty());
        assert!(z
            .render()
            .contains("second sentence nobody needs up front."));
    }

    #[test]
    fn zoom_carries_literals_wires_and_reverse_edges() {
        let rendered = projector(TINY).zoom(&Selection::names(["/osc"])).render();
        assert_eq!(
            rendered,
            "/osc oscillator\ndoc: the tone\nin: freq=220, waveform=Saw\nout: audio->/amp.a"
        );
    }

    /// The blast radius of `/amp` includes the interface output pipe it feeds — remove the node
    /// and the pipe breaks, so the pipe is a consumer like any node input.
    #[test]
    fn an_output_pipe_is_a_consumer_too() {
        let rendered = projector(TINY).zoom(&Selection::names(["/amp"])).render();
        assert!(rendered.contains("out: out->pipe:out"), "{rendered}");
    }

    /// A consumer that wrote the sole-output sugar (`"/lvl"`, no port) still reports which port
    /// the edge leaves from — resolved against the registry, not guessed.
    #[test]
    fn sole_output_sugar_resolves_to_the_port_name() {
        let rendered = projector(TINY).zoom(&Selection::names(["/lvl"])).render();
        assert!(rendered.contains("out: out->/amp.b"), "{rendered}");
        assert!(rendered.contains("in: in<-/level"), "{rendered}");
    }

    #[test]
    fn zoom_by_type_multi_matches_and_reports_a_type_nobody_has() {
        let p = projector(TINY);
        let hit = p.zoom(&Selection::Type("m2s".into()));
        assert_eq!(hit.nodes.len(), 1);
        let miss = p.zoom(&Selection::Type("reverb".into()));
        assert!(miss.nodes.is_empty());
        assert_eq!(miss.unmatched, ["reverb"]);
    }

    /// A name that matches nothing is *reported*. Silence here would be the exact failure this
    /// surface exists to prevent.
    #[test]
    fn an_unmatched_address_is_never_silent() {
        let z = projector(TINY).zoom(&Selection::names(["/osc", "/nope"]));
        assert_eq!(z.nodes.len(), 1);
        assert_eq!(z.unmatched, ["/nope"]);
        assert!(z.render().ends_with("no match: /nope"));
    }

    #[test]
    fn pipes_render_range_curve_unit_default_and_channel() {
        let p = projector(TINY);
        assert_eq!(
            p.pipes(&Selection::All).render(),
            "pipes in (1):\nlevel:f32 dB exp 0..1=0.5\npipes out (1):\nout<-/amp ch0"
        );
        // The type predicate cuts input pipes (an output pipe declares no type of its own) — and
        // the render says how much it cut, so `pipes in (1):` is never read as the document total.
        let by_type = p.pipes(&Selection::Type("f32".into()));
        assert_eq!(by_type.inputs.len(), 1);
        assert!(by_type.outputs.is_empty());
        assert!(
            by_type.render().starts_with("pipes (1 of 2 shown):"),
            "{}",
            by_type.render()
        );
        // A selection that filters everything out must not claim the document has no interface —
        // it has one, the agent just asked for the wrong name.
        let miss = p.pipes(&Selection::names(["nope"]));
        assert_eq!(miss.unmatched, ["nope".to_string()]);
        assert_eq!(miss.render(), "pipes (0 of 2 shown):\nno match: nope");
    }

    #[test]
    fn a_dark_resource_marks_the_index_and_explains_itself_in_the_views() {
        let p = projector(DARK);
        assert!(p.index().render().contains("/v voicer dark:voice"));
        let zoom = p.zoom(&Selection::names(["/v"])).render();
        assert!(zoom.contains("config: voices=2"), "{zoom}");
        assert!(zoom.contains("res: voice=missing-voice DARK:"), "{zoom}");
        // The table row nobody references is still listed; the dangling ref gets its own line.
        let res = p.resources().render();
        assert!(res.contains("unused -> nowhere.json unreferenced"), "{res}");
        // ...and it names the node making it, which is the node the agent has to go fix.
        assert!(
            res.contains("dangling voice=missing-voice on /v DARK:"),
            "{res}"
        );
    }

    /// The projection is not a second validation authority: a document that will not load still
    /// projects, and says so.
    #[test]
    fn a_document_that_does_not_load_still_projects() {
        const BROKEN: &str = r#"{
            "format_version": 3,
            "instrument": "broken",
            "nodes": [ {"type": "oscillator", "address": "/osc",
                        "inputs": {"freq": {"from": "/nope.out"}}} ]
        }"#;
        let index = projector(BROKEN).index().render();
        assert!(index.contains("DOES NOT LOAD — run validate"), "{index}");
        assert!(index.contains("/osc oscillator"), "{index}");
    }

    /// A node can carry more than one resource reference, and the index's dark marker scans all of
    /// them — so the zoom has to as well, or the index says "dark" while the zoom shows a healthy
    /// resource and never mentions the broken one.
    #[test]
    fn a_zoom_lists_every_resource_ref_the_index_marked_on() {
        const TWO_REFS: &str = r#"{
            "format_version": 3,
            "instrument": "two-refs",
            "resources": { "kit": "kit.wav" },
            "nodes": [
                {"type": "subpatch", "address": "/n", "sample": "kit", "patch": "nowhere"}
            ]
        }"#;
        let p = projector(TWO_REFS);
        assert!(p.index().render().contains("/n subpatch dark:patch"));
        let zoom = p.zoom(&Selection::names(["/n"])).render();
        assert!(zoom.contains("sample=kit -> kit.wav"), "{zoom}");
        assert!(zoom.contains("patch=nowhere DARK:"), "{zoom}");
    }

    /// A child whose *reference* is fine but whose *document* will not describe leaves no boundary.
    /// The dark `res:` line does not cover that case — the reference resolved — so the absence gets
    /// its own note. Dropping it is the silent omission this surface exists to prevent.
    #[test]
    fn a_child_that_will_not_describe_says_so_instead_of_vanishing() {
        const HOST: &str = r#"{
            "format_version": 3,
            "instrument": "host",
            "resources": { "broken": "broken.json" },
            "nodes": [ {"type": "voicer", "address": "/v", "voice": "broken"} ]
        }"#;
        /// Hands back a document no loader will accept.
        struct BrokenChild;
        impl ResourceResolver for BrokenChild {
            fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
                Err(ResolveError::NotFound(source.to_string()))
            }
            fn resolve_text(&self, _: &str) -> Result<String, ResolveError> {
                Ok("{ not an instrument".to_string())
            }
        }
        let registry = Registry::builtin();
        let p = Projector::new(HOST, &registry, &BrokenChild).expect("mints");
        let zoom = p.zoom(&Selection::names(["/v"])).render();
        assert!(zoom.contains("res: voice=broken -> broken.json"), "{zoom}");
        assert!(zoom.contains("note: no boundary:"), "{zoom}");
    }

    /// Every view answers in sentences. An empty string is indistinguishable from a truncated or
    /// failed call, and a rig with no `interface` is an ordinary document.
    #[test]
    fn a_document_with_no_interface_still_answers_the_pipe_view() {
        const NO_IFACE: &str = r#"{
            "format_version": 3,
            "instrument": "bare",
            "nodes": [ {"type": "oscillator", "address": "/osc"} ]
        }"#;
        assert_eq!(
            projector(NO_IFACE).pipes(&Selection::All).render(),
            "pipes (0): this document declares no interface"
        );
    }

    /// The resources header counts what the block actually lists, and dangling refs — a different
    /// grammar from the table rows — are labelled as such.
    #[test]
    fn the_resources_header_counts_dangling_refs_too() {
        let rendered = projector(DARK).resources().render();
        let mut lines = rendered.lines();
        assert_eq!(lines.next().unwrap(), "resources (1 listed, 1 dangling):");
        assert_eq!(lines.clone().count(), 2);
        assert!(lines.any(|l| l.starts_with("dangling voice=missing-voice")));
    }

    /// The `[+N chars]` budget the agent spends its turn on counts **characters**; this codebase's
    /// prose is full of multi-byte punctuation, and bytes would inflate it.
    #[test]
    fn the_remaining_doc_budget_is_counted_in_characters() {
        const EMDASH: &str = r#"{
            "format_version": 3,
            "instrument": "wide",
            "doc": "First. \u2014\u2014\u2014\u2014",
            "nodes": [ {"type": "oscillator", "address": "/osc"} ]
        }"#;
        // " ————" after the role line: one space + four 3-byte em-dashes = 5 chars, 13 bytes.
        assert!(
            projector(EMDASH).index().render().contains("[+5 chars"),
            "{}",
            projector(EMDASH).index().render()
        );
    }

    /// A view whose job is resolved/dark state must not report *unchecked* as *fine*. Resolution
    /// is discovered by loading; when the load never completed, the honest answer is a caveat.
    #[test]
    fn resolution_state_is_caveated_when_the_document_does_not_load() {
        // Same missing resource in both, but the second also carries a broken wire, so it never
        // loads and nothing ever checked the resource.
        const LOADS: &str = r#"{
            "format_version": 3,
            "instrument": "loads",
            "resources": { "kit": "nowhere.wav" },
            "nodes": [ {"type": "sample", "address": "/sp", "sample": "kit"} ]
        }"#;
        const BROKEN: &str = r#"{
            "format_version": 3,
            "instrument": "broken",
            "resources": { "kit": "nowhere.wav" },
            "nodes": [
                {"type": "sample", "address": "/sp", "sample": "kit"},
                {"type": "oscillator", "address": "/osc", "inputs": {"freq": {"from": "/nope.out"}}}
            ]
        }"#;
        let good = projector(LOADS);
        assert!(good.resources().render().contains("DARK:"));
        assert!(!good.resources().render().contains(LOAD_CAVEAT));

        let bad = projector(BROKEN);
        let res = bad.resources().render();
        assert!(res.starts_with(LOAD_CAVEAT), "{res}");
        // ...and the same caveat rides a zoom, whose `res:` line reports the same unchecked state.
        let zoom = bad.zoom(&Selection::names(["/sp"])).render();
        assert!(zoom.starts_with(LOAD_CAVEAT), "{zoom}");
    }

    /// Every view answers in sentences — including a zoom of a document with no nodes, which is
    /// the very first document an agent creates.
    #[test]
    fn a_zoom_never_renders_the_empty_string() {
        const FRESH: &str = r#"{"format_version": 3, "instrument": "fresh", "nodes": []}"#;
        assert_eq!(
            projector(FRESH).zoom(&Selection::All).render(),
            "nodes (0): this document has no nodes"
        );
        // A document that HAS nodes, asked for none of them, says so differently.
        assert_eq!(
            projector(TINY).zoom(&Selection::Names(Vec::new())).render(),
            "nodes (0 of 3 shown)"
        );
    }

    /// The projection is a grammar an agent parses, and nothing validates a document's spelling
    /// against it. A node addressed `"in: freq=999"` loads cleanly through the real engine path;
    /// unquoted, it would forge a line indistinguishable from a real input binding.
    #[test]
    fn a_document_cannot_forge_projection_structure_with_its_own_names() {
        const FORGED: &str = r#"{
            "format_version": 3,
            "instrument": "for\nged",
            "resources": { "kit\nfake -> evil.wav sample:/x": "boom\nresources (0):" },
            "interface": {
                "inputs": { "lvl\nfake:f32 0..1=0": {"type": "f32", "default": 0.5} }
            },
            "nodes": [
                {"type": "oscillator", "address": "in: freq=999"},
                {"type": "m2s", "address": "/kick drum"}
            ]
        }"#;
        let p = projector(FORGED);
        let index = p.index().render();
        // The forging document still loads — quoting is the only thing standing between the
        // agent and a phantom record.
        assert!(!index.contains("DOES NOT LOAD"), "{index}");
        // The newline in the instrument name is escaped, not emitted — it cannot split the header.
        assert!(index.contains(r#"instrument "for\nged""#), "{index}");
        assert!(index.contains(r#""in: freq=999" oscillator"#), "{index}");
        assert!(index.contains(r#""/kick drum" m2s"#), "{index}");
        // One line per node plus the one-line header: nothing forged a record of its own.
        assert_eq!(index.lines().count(), 3);
        // ...and the same holds for every OTHER view. Asserting this on the index alone is how
        // three forgeable paths survived a review round — and a fixture with no `interface` and no
        // `resources` makes the pipe and resource arms vacuous, which is how they survived
        // another. Each view below has real, hostile content to render.
        assert_eq!(p.zoom(&Selection::All).render().lines().count(), 2);
        // Header + the one pipe. The pipe's own name would otherwise forge a second pipe row.
        assert_eq!(p.pipes(&Selection::All).render().lines().count(), 2);
        // Header + the one entry. Its id and its source would each forge a row.
        assert_eq!(p.resources().render().lines().count(), 2);
        // A selection term is caller text, and a door forwards agent- or user-authored names.
        assert_eq!(
            p.zoom(&Selection::names(["no\nmatch: forged"]))
                .render()
                .lines()
                .count(),
            1
        );
    }

    /// The boundary line is the one record assembled from a **second document**, so the child
    /// chooses the strings in it. A child pipe named `"gate\nout: …"` would otherwise forge a
    /// phantom consumer on the *parent* node — a lie about the parent's blast radius, read off a
    /// document the agent never asked about.
    #[test]
    fn a_hostile_child_cannot_forge_lines_in_its_hosts_zoom() {
        const HOST: &str = r#"{
            "format_version": 3,
            "instrument": "host",
            "resources": { "v": "voice.json" },
            "nodes": [ {"type": "voicer", "address": "/v", "voice": "v"} ]
        }"#;
        const EVIL_CHILD: &str = r#"{
            "format_version": 3,
            "instrument": "evil\nin: freq=999",
            "interface": {
                "inputs": { "gate\nout: audio->/x.y": {"type": "f32", "default": 1.0} },
                "outputs": { "audio": {"from": "/osc.audio"} }
            },
            "nodes": [ {"type": "oscillator", "address": "/osc"} ]
        }"#;
        struct EvilChild;
        impl ResourceResolver for EvilChild {
            fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
                Err(ResolveError::NotFound(source.to_string()))
            }
            fn resolve_text(&self, _: &str) -> Result<String, ResolveError> {
                Ok(EVIL_CHILD.to_string())
            }
        }
        let registry = Registry::builtin();
        let p = Projector::new(HOST, &registry, &EvilChild).expect("mints");
        let zoom = p.zoom(&Selection::names(["/v"])).render();
        assert!(zoom.contains("boundary:"), "{zoom}");
        // address line + res line + boundary line. Nothing else.
        assert_eq!(zoom.lines().count(), 3, "GOT:\n{zoom}");
    }

    /// A `subpatch` with no `patch` reference is dark with no id to blame it on — there is no
    /// `res:` line to carry the reason, so without a note the index says `dark:patch` and the zoom
    /// the agent goes to next shows a clean node.
    #[test]
    fn a_node_the_loader_called_dark_explains_itself_in_the_zoom() {
        const NO_REF: &str = r#"{
            "format_version": 3,
            "instrument": "no-ref",
            "nodes": [ {"type": "subpatch", "address": "/sub"} ]
        }"#;
        let p = projector(NO_REF);
        assert!(p.index().render().contains("/sub subpatch dark:patch"));
        let zoom = p.zoom(&Selection::names(["/sub"])).render();
        assert!(zoom.contains("note:"), "{zoom}");
    }

    /// The load caveat is not an answer, so it must not count as one. Prepending it before the
    /// "nothing to show" check swallowed the sentence on exactly the documents that need it.
    #[test]
    fn the_load_caveat_does_not_swallow_the_nothing_to_show_sentence() {
        const BROKEN_EMPTY: &str = r#"{
            "format_version": 3,
            "instrument": "broken-empty",
            "interface": { "outputs": { "out": {"from": "/nope.audio"} } },
            "nodes": []
        }"#;
        let rendered = projector(BROKEN_EMPTY).zoom(&Selection::All).render();
        assert_eq!(
            rendered,
            format!("{LOAD_CAVEAT}\nnodes (0): this document has no nodes")
        );
    }

    /// An empty or whitespace-only `doc` is not intent, and a bare `doc:` line is a wasted line on
    /// the highest-frequency read.
    #[test]
    fn a_blank_doc_emits_no_doc_line() {
        const BLANK: &str = r#"{
            "format_version": 3,
            "instrument": "blank",
            "doc": "   ",
            "nodes": []
        }"#;
        let p = projector(BLANK);
        assert!(!p.index().render().contains("doc:"));
        assert!(!p.zoom(&Selection::names(["/"])).render().contains("doc:"));
    }

    /// The memoized boundary is cut once per **source**, not per referencing node: two voicers on
    /// one voice document, and two resource ids pointing at one source, describe it once.
    #[test]
    fn a_child_boundary_is_described_once_per_source() {
        const SHARED: &str = r#"{
            "format_version": 3,
            "instrument": "shared",
            "resources": { "a": "voice.json", "b": "voice.json" },
            "nodes": [
                {"type": "voicer", "address": "/v1", "voice": "a"},
                {"type": "voicer", "address": "/v2", "voice": "a"},
                {"type": "voicer", "address": "/v3", "voice": "b"}
            ]
        }"#;
        const VOICE: &str = r#"{
            "format_version": 3,
            "instrument": "voice",
            "interface": {
                "inputs": { "freq": {"type": "f32", "default": 440.0} },
                "outputs": { "audio": {"from": "/osc.audio"} }
            },
            "nodes": [ {"type": "oscillator", "address": "/osc", "inputs": {"freq": {"from": "/freq"}}} ]
        }"#;
        /// Counts how many times the child document is actually read.
        struct Counting(std::cell::Cell<usize>);
        impl ResourceResolver for Counting {
            fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
                Err(ResolveError::NotFound(source.to_string()))
            }
            fn resolve_text(&self, _: &str) -> Result<String, ResolveError> {
                self.0.set(self.0.get() + 1);
                Ok(VOICE.to_string())
            }
        }
        let registry = Registry::builtin();
        let resolver = Counting(std::cell::Cell::new(0));
        let p = Projector::new(SHARED, &registry, &resolver).expect("mints");
        // Force the lazy load first — it reads the children itself; what is being counted here is
        // the *boundary* describe, which is a second, separate read of the same documents.
        p.index();
        let before = resolver.0.get();
        let all = p.zoom(&Selection::All);
        // Three voicers, one source — and zooming again costs nothing more.
        assert_eq!(all.nodes.iter().filter(|n| n.boundary.is_some()).count(), 3);
        assert_eq!(resolver.0.get() - before, 1);
        p.zoom(&Selection::All);
        assert_eq!(resolver.0.get() - before, 1);
    }

    /// Nothing forbids a `.` in a node address — a v1 interface entry named `"my.tone"` migrates
    /// to a pipe minting `/my.tone` verbatim — so the loader resolves an exact address BEFORE the
    /// last-`.` split, and the reverse edges must too. Splitting first fabricates a consumer on
    /// `/my` and hides the real one on `/my.tone`: `remove_node /my.tone` would be reported as
    /// breaking nothing while it breaks `/lvl`.
    #[test]
    fn a_dotted_address_is_matched_whole_before_the_last_dot_split() {
        const DOTTED: &str = r#"{
            "format_version": 3,
            "instrument": "dotted",
            "nodes": [
                {"type": "oscillator", "address": "/my"},
                {"type": "oscillator", "address": "/my.tone"},
                {"type": "mul_f32_signal", "address": "/lvl",
                 "inputs": {"a": {"from": "/my.tone"}}}
            ]
        }"#;
        let p = projector(DOTTED);
        // The document loads, so this is a real wire the engine makes — not a malformed edge case.
        assert!(!p.index().render().contains("DOES NOT LOAD"));
        let z = p.zoom(&Selection::names(["/my", "/my.tone"]));
        let real = z.nodes.iter().find(|n| n.address == "/my.tone").unwrap();
        assert_eq!(real.consumers.len(), 1, "{:?}", real.consumers);
        assert_eq!(real.consumers[0].port.as_deref(), Some("audio"));
        let bystander = z.nodes.iter().find(|n| n.address == "/my").unwrap();
        assert!(bystander.consumers.is_empty(), "{:?}", bystander.consumers);
    }

    /// The addressable set is the loader's, not an approximation of it. A node addressed
    /// slashlessly (`my.audio`) is legal and loads; the loader's `by_addr` holds it verbatim, so
    /// `{"from": "/my.audio"}` does **not** whole-match and splits into `/my` port `audio`. A
    /// projection that whole-matched anyway would file the edge under a key no node has, and the
    /// consumer would vanish from the reverse-edge view altogether.
    #[test]
    fn a_slashless_address_does_not_whole_match_a_slashed_reference() {
        const SLASHLESS: &str = r#"{
            "format_version": 3,
            "instrument": "slashless",
            "nodes": [
                {"type": "oscillator", "address": "/my"},
                {"type": "oscillator", "address": "my.audio"},
                {"type": "mul_f32_signal", "address": "/lvl",
                 "inputs": {"a": {"from": "/my.audio"}}}
            ]
        }"#;
        let p = projector(SLASHLESS);
        // The engine accepts this document, so the edge it makes is the edge to report.
        assert!(!p.index().render().contains("DOES NOT LOAD"));
        let z = p.zoom(&Selection::All);
        let by = |addr: &str| {
            z.nodes
                .iter()
                .find(|n| n.address == addr)
                .unwrap()
                .consumers
                .clone()
        };
        assert_eq!(by("/my").len(), 1, "{:?}", by("/my"));
        assert_eq!(by("/my")[0].port.as_deref(), Some("audio"));
        assert!(by("my.audio").is_empty(), "{:?}", by("my.audio"));
    }

    /// A `subpatch` declares no outputs of its own — its ports are its child's face — so sugar
    /// into one cannot be resolved from the registry. The answer is on the boundary line directly
    /// below, so printing `?` would be withholding something already in hand.
    #[test]
    fn sole_output_sugar_into_a_nest_resolves_from_the_childs_face() {
        const HOST: &str = r#"{
            "format_version": 3,
            "instrument": "host",
            "resources": { "c": "child.json" },
            "nodes": [
                {"type": "subpatch", "address": "/s", "patch": "c"},
                {"type": "mul_f32_signal", "address": "/lvl", "inputs": {"a": {"from": "/s"}}}
            ]
        }"#;
        const CHILD: &str = r#"{
            "format_version": 3,
            "instrument": "child",
            "interface": { "outputs": { "audio": {"from": "/osc.audio"} } },
            "nodes": [ {"type": "oscillator", "address": "/osc"} ]
        }"#;
        struct Child;
        impl ResourceResolver for Child {
            fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
                Err(ResolveError::NotFound(source.to_string()))
            }
            fn resolve_text(&self, _: &str) -> Result<String, ResolveError> {
                Ok(CHILD.to_string())
            }
        }
        let registry = Registry::builtin();
        let p = Projector::new(HOST, &registry, &Child).expect("mints");
        let zoom = p.zoom(&Selection::names(["/s"])).render();
        assert!(zoom.contains("out: audio->/lvl.a"), "{zoom}");
        assert!(!zoom.contains("?->"), "{zoom}");
    }

    /// The caveat rides the view, not the header's absence: the header's `DOES NOT LOAD` says the
    /// document is broken, not that every `res:` line below it went unchecked. Making one depend
    /// on the other meant adding `/` to a selection silently downgraded what the agent was told.
    #[test]
    fn asking_for_the_document_too_does_not_suppress_the_load_caveat() {
        const BROKEN: &str = r#"{
            "format_version": 3,
            "instrument": "broken",
            "resources": { "kit": "nowhere.wav" },
            "nodes": [
                {"type": "sample", "address": "/sp", "sample": "kit"},
                {"type": "oscillator", "address": "/osc", "inputs": {"freq": {"from": "/nope.out"}}}
            ]
        }"#;
        let p = projector(BROKEN);
        assert!(p
            .zoom(&Selection::names(["/sp"]))
            .render()
            .starts_with(LOAD_CAVEAT));
        assert!(p
            .zoom(&Selection::names(["/", "/sp"]))
            .render()
            .starts_with(LOAD_CAVEAT));
    }

    /// The completeness guard: walk the **real** format types and prove every leaf field is
    /// consciously dispositioned by [`FIELD_COVERAGE`]. A new format field with no view fails
    /// here, which is the whole point — a read surface this permanent cannot be defended by
    /// vigilance.
    #[cfg(feature = "schemars")]
    mod coverage {
        use super::*;
        use crate::format::InstrumentDoc;
        use serde_json::{Map, Value};

        /// Walk a schemars (draft 2020-12) schema into a flat, sorted set of leaf field-paths.
        /// Convention: object properties → `parent.field`; arrays → `parent[]`; maps
        /// (`additionalProperties` schema) → `parent{}`; untagged enums / `Option`
        /// (`anyOf`/`oneOf`) → union the branches under the same path. `$ref` resolves into
        /// `$defs`, with a visited guard so a (hypothetical) recursive type terminates.
        fn walk(
            node: &Value,
            defs: &Map<String, Value>,
            path: &str,
            out: &mut BTreeSet<String>,
            active: &mut BTreeSet<String>,
        ) {
            // A `{"type": "null"}` branch (the None side of an Option) carries no field.
            if node.get("type").and_then(Value::as_str) == Some("null") {
                return;
            }
            // Every structural keyword on this node is followed, and NONE of them short-circuits
            // the others — `$ref` included, which schemars already emits with siblings (a `$ref`
            // beside a `description`). A guard that under-enumerates stays green while the format
            // grows a field with no view, the exact failure it exists to make impossible.
            let mut structural = false;
            if let Some(r) = node.get("$ref").and_then(Value::as_str) {
                let name = r.rsplit('/').next().unwrap_or(r).to_string();
                if active.insert(name.clone()) {
                    if let Some(def) = defs.get(&name) {
                        walk(def, defs, path, out, active);
                    }
                    active.remove(&name);
                } else {
                    out.insert(format!("{path} → <recursive {name}>"));
                }
                structural = true;
            }
            for key in ["anyOf", "oneOf", "allOf"] {
                for sub in node
                    .get(key)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if sub.get("type").and_then(Value::as_str) == Some("null") {
                        continue;
                    }
                    walk(sub, defs, path, out, active);
                    structural = true;
                }
            }
            for (k, v) in node
                .get("properties")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                let child = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                walk(v, defs, &child, out, active);
                structural = true;
            }
            if let Some(ap) = node.get("additionalProperties") {
                if ap.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                    walk(ap, defs, &format!("{path}{{}}"), out, active);
                    structural = true;
                }
            }
            if let Some(items) = node.get("items") {
                walk(items, defs, &format!("{path}[]"), out, active);
                structural = true;
            }
            if !structural {
                out.insert(path.to_string());
            }
        }

        /// Every leaf field-path of the real [`InstrumentDoc`], enumerated mechanically — zero
        /// hand-maintenance, which is what makes the guard trustworthy.
        fn format_fields() -> BTreeSet<String> {
            let schema: Value =
                serde_json::to_value(schemars::schema_for!(InstrumentDoc)).expect("schema");
            let defs = schema
                .get("$defs")
                .or_else(|| schema.get("definitions"))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut out = BTreeSet::new();
            walk(&schema, &defs, "", &mut out, &mut BTreeSet::new());
            out
        }

        fn undispositioned(fields: &BTreeSet<String>) -> Vec<String> {
            let table: BTreeSet<String> =
                FIELD_COVERAGE.iter().map(|(k, _)| k.to_string()).collect();
            let mut problems: Vec<String> = fields
                .difference(&table)
                .map(|f| format!("UNDISPOSITIONED format field (silent-omission risk): {f}"))
                .collect();
            problems.extend(
                table
                    .difference(fields)
                    .map(|k| format!("STALE coverage row (field no longer in the format): {k}")),
            );
            problems
        }

        #[test]
        fn every_format_field_is_dispositioned_into_a_view() {
            let problems = undispositioned(&format_fields());
            assert!(
                problems.is_empty(),
                "the projection's coverage table and the format have diverged.\n\
                 Give the new field a view in `FIELD_COVERAGE` (or an explicit `omit:` reason):\n  {}",
                problems.join("\n  ")
            );
        }

        /// The guard has teeth: a future field landing with no view goes red. Without this the
        /// test above could pass by walking nothing.
        #[test]
        fn a_new_field_with_no_view_fails_the_guard() {
            let mut future = format_fields();
            future.insert("nodes[].tempo_hint".to_string());
            assert_eq!(undispositioned(&future).len(), 1);
        }

        /// ...and the walker itself does not under-enumerate. The teeth test above injects into the
        /// already-walked set, so it cannot catch a walker that skipped a subtree; this walks a
        /// schema whose node carries `allOf` **beside** `properties` — a shape schemars is free to
        /// emit — and asserts both branches are enumerated. A walker that short-circuits on the
        /// first structural keyword silently stops covering whole types while staying green, which
        /// is the one thing this guard cannot be allowed to do.
        #[test]
        fn the_walker_follows_sibling_keywords_rather_than_the_first_one() {
            let schema: Value = serde_json::json!({
                "properties": { "direct": { "type": "string" } },
                "allOf": [ { "properties": { "merged": { "type": "string" } } } ]
            });
            let mut out = BTreeSet::new();
            walk(&schema, &Map::new(), "", &mut out, &mut BTreeSet::new());
            assert!(out.contains("direct"), "{out:?}");
            assert!(out.contains("merged"), "{out:?}");
        }

        /// ...and `$ref` is one of those keywords, not an early exit. schemars already emits a
        /// `$ref` beside a sibling (`description`), so a `$ref` that returned before the rest
        /// would be a subtree the guard stops covering the moment that sibling is structural.
        #[test]
        fn the_walker_follows_keywords_sitting_beside_a_ref() {
            let mut defs = Map::new();
            defs.insert(
                "Inner".to_string(),
                serde_json::json!({"properties": {"via_ref": {"type": "string"}}}),
            );
            let schema: Value = serde_json::json!({
                "$ref": "#/$defs/Inner",
                "properties": { "beside_ref": { "type": "string" } }
            });
            let mut out = BTreeSet::new();
            walk(&schema, &defs, "", &mut out, &mut BTreeSet::new());
            assert!(out.contains("via_ref"), "{out:?}");
            assert!(out.contains("beside_ref"), "{out:?}");
        }
    }
}
