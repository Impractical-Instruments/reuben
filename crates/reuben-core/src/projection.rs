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
//!   sources), its `config` constants, its `doc`, its resource ref, a nested child's boundary, and
//!   **its consumers** — the reverse edges, without which the agent is blind to the blast radius of
//!   every destructive verb.
//! - [`Projector::pipes`] — the `interface` boundary: each pipe's type/range/curve/unit/channel.
//! - [`Projector::resources`] — the `id → source` table, who references each id, and whether it
//!   resolved.
//!
//! Every view is a pure function of the document (plus the registry and the resolver), and none of
//! them judges the document: `validate` remains the single authority on whether it loads
//! (`#loader-single-authority`). A document that fails to load still projects — going blind exactly
//! when the agent needs to see is the worst possible failure — with `loadable: false` in the header
//! saying so.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::format::{
    load_instrument_doc, ConfigValue, InputValue, InterfaceEntry, LoadWarning, NodeDoc,
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
`port->pipe:name` (the blast radius of removing the node), `res:` its resource ref, `boundary:` a \
nested child's pipes. Pipes: `name:type unit exp lo..hi=default chN` for an input pipe, \
`name<-/node.port chN` for an output pipe. Zoom `/` for the document's full doc text.";

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
            Scalar::Symbol(s) => s.clone(),
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
            self.instrument, self.nodes, self.format_version, self.projection_version
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
        if let Some(doc) = self.doc.as_deref() {
            let role = first_sentence(doc);
            let rest = doc.split_whitespace().collect::<Vec<_>>().join(" ").len() - role.len();
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
        if let Some(doc) = self.doc.as_deref() {
            let one_line = doc.split_whitespace().collect::<Vec<_>>().join(" ");
            out.push_str(&format!("\ndoc: {one_line}"));
        }
        out
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
            out.push_str(&format!("\n{} {}", n.address, n.type_name));
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
            (Some(src), _) => format!("{}<-{src}", self.name),
            (None, Some(v)) => format!("{}={}", self.name, v.render()),
            // Unreachable through `InputValue` (a binding is a wire or a literal), rendered rather
            // than panicked so a future third form degrades visibly instead of vanishing.
            (None, None) => format!("{}=?", self.name),
        }
    }
}

/// What one of a node's outputs feeds — the reverse edge. Mandatory, not a nicety: consumers are
/// otherwise derivable only by scanning every node, which the index deliberately does not carry,
/// so without this the agent cannot see what a `remove_node` would break.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct OutEdge {
    /// The output port this edge leaves from. `None` when the consumer used the sole-output sugar
    /// *and* the port could not be resolved from the registry (an unknown operator type).
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
        let port = self.port.as_deref().unwrap_or("?");
        match &self.node {
            Some(node) => format!("{port}->{node}.{}", self.input),
            None => format!("{port}->pipe:{}", self.input),
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
    /// The `resources` table's source for this id — `None` when the id is not in the table.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Why the reference is dark, when it is. `None` is resolved.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dark: Option<String>,
}

impl ResourceRef {
    fn render(&self) -> String {
        let mut s = format!("{}={}", self.slot, self.id);
        if let Some(src) = &self.source {
            s.push_str(&format!(" -> {src}"));
        }
        if let Some(why) = &self.dark {
            s.push_str(&format!(" DARK: {why}"));
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceRef>,
    /// A nested child's face — **never its internals**. The format is not recursive (a child is a
    /// resource id), so the projection mirrors the *document*: an opaque node plus the boundary it
    /// presents. The child's own nodes are reached by projecting the child document.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub boundary: Option<PatchBoundary>,
}

impl NodeZoom {
    /// One node's block: the header line, then only the lines it has anything to say on.
    pub fn render(&self) -> String {
        let mut out = format!("{} {}", self.address, self.type_name);
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
        if let Some(r) = &self.resource {
            line("res", r.render());
        }
        if let Some(b) = &self.boundary {
            line("boundary", render_boundary(b));
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
    /// Selection terms that matched nothing. Reported, never silent — "no such node" is a fact the
    /// agent's next verb depends on.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched: Vec<String>,
}

impl Zoom {
    pub fn render(&self) -> String {
        let mut blocks: Vec<String> = Vec::new();
        if let Some(h) = &self.header {
            blocks.push(h.render_full());
        }
        blocks.extend(self.nodes.iter().map(NodeZoom::render));
        if !self.unmatched.is_empty() {
            blocks.push(format!("no match: {}", self.unmatched.join(", ")));
        }
        blocks.join("\n")
    }
}

/// One `interface` pipe. Both directions share this shape because [`InterfaceEntry`] is one
/// untagged union — either pipe form can appear in either map, and a reader that assumed otherwise
/// would be one malformed document away from lying.
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
        let mut s = self.name.clone();
        if let Some(ty) = &self.ty {
            s.push_str(&format!(":{ty}"));
        }
        if let Some(from) = &self.from {
            s.push_str(&format!("<-{from}"));
        }
        if let Some(t) = &self.v1_target {
            s.push_str(&format!(" v1-target:{t}"));
        }
        if let Some(u) = &self.unit {
            s.push_str(&format!(" {u}"));
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
    /// Selection terms that matched no pipe in either map.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unmatched: Vec<String>,
}

impl PipeView {
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (label, pipes) in [("in", &self.inputs), ("out", &self.outputs)] {
            if pipes.is_empty() {
                continue;
            }
            out.push_str(&format!("pipes {label} ({}):", pipes.len()));
            for p in pipes.iter() {
                out.push_str(&format!("\n{}", p.render()));
            }
            out.push('\n');
        }
        if !self.unmatched.is_empty() {
            out.push_str(&format!("no match: {}\n", self.unmatched.join(", ")));
        }
        out.pop();
        out
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
    /// `"{slot}:{node address}"` per referencing node, in document order. Empty means the entry is
    /// unreferenced — legal, and ignored by the loader, but worth seeing.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
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
}

impl ResourcesView {
    pub fn render(&self) -> String {
        let mut lines = vec![format!("resources ({}):", self.entries.len())];
        for e in &self.entries {
            let mut s = format!("{} -> {}", e.id, e.source);
            if e.refs.is_empty() {
                s.push_str(" unreferenced");
            } else {
                s.push_str(&format!(" {}", e.refs.join(", ")));
            }
            if let Some(why) = &e.dark {
                s.push_str(&format!(" DARK: {why}"));
            }
            lines.push(s);
        }
        for d in &self.dangling {
            lines.push(d.render());
        }
        lines.join("\n")
    }
}

/// The projection source: one mint, one load, from which every view is cut. Doors hold this for a
/// turn so a verb can echo the zoom of what it touched without re-parsing.
pub struct Projector<'a> {
    doc: NormalizedDoc,
    registry: &'a Registry,
    resolver: &'a dyn ResourceResolver,
    loadable: bool,
    /// Resource ids that did not resolve this load → why. Empty when the document did not load at
    /// all (the load is what discovers darkness).
    dark_ids: BTreeMap<String, String>,
    /// Node addresses the loader itself called dark for a reason that is not an id — today only a
    /// `subpatch` carrying no `patch` reference.
    dark_nodes: BTreeMap<String, String>,
    /// `source node address` → its consumers. Built once: every node's inputs, plus every output
    /// pipe's feed, inverted.
    consumers: BTreeMap<String, Vec<OutEdge>>,
}

impl<'a> Projector<'a> {
    /// Mint the document and load it once. The mint is required — without a parseable document
    /// there is no structure to project — but the **load is best-effort**: a document that fails to
    /// load still projects, with `loadable: false` in the header and no dark-resource markers,
    /// because `validate` is the single authority on validity and going blind is the worst way to
    /// report invalidity.
    pub fn new(
        json: &str,
        registry: &'a Registry,
        resolver: &'a dyn ResourceResolver,
    ) -> Result<Self, String> {
        // The mint is the parse + version gate + migration; its failure is a document with no
        // shape to read, so this is the one thing the projection cannot degrade past.
        let doc =
            NormalizedDoc::from_json(json, registry, Some(resolver)).map_err(|e| e.to_string())?;
        let loaded = load_instrument_doc(&doc, registry, resolver).ok();
        let mut dark_ids = BTreeMap::new();
        let mut dark_nodes = BTreeMap::new();
        for w in loaded.iter().flat_map(|l| l.warnings.iter()) {
            collect_dark(w, &mut dark_ids, &mut dark_nodes);
        }
        let consumers = build_consumers(&doc, registry);
        Ok(Projector {
            doc,
            registry,
            resolver,
            loadable: loaded.is_some(),
            dark_ids,
            dark_nodes,
            consumers,
        })
    }

    fn header(&self) -> DocHeader {
        DocHeader {
            instrument: self.doc.instrument.clone(),
            format_version: self.doc.format_version,
            projection_version: PROJECTION_VERSION,
            nodes: self.doc.nodes.len(),
            doc: self.doc.doc.clone(),
            loadable: self.loadable,
        }
    }

    /// The dark marker for one node: the first resource slot whose reference did not resolve, or
    /// the loader's own node-level darkness.
    fn dark_slot(&self, node: &NodeDoc) -> Option<String> {
        for (slot, id) in resource_refs(node) {
            // Either the load said the id went dark, or the document itself has no row for it —
            // the second is true whether or not the document loaded, so the marker survives a
            // `loadable: false` projection.
            if self.dark_ids.contains_key(id) || !self.doc.resources.contains_key(id) {
                return Some(slot.to_string());
            }
        }
        // A `subpatch` carrying no `patch` reference at all: dark with no id to blame it on.
        self.dark_nodes
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
            unmatched,
        }
    }

    fn zoom_node(&self, i: usize) -> NodeZoom {
        let n = &self.doc.nodes[i];
        let resource = resource_refs(n).into_iter().next().map(|(slot, id)| {
            let source = self.doc.resources.get(id).cloned();
            let dark = self.dark_ids.get(id).cloned().or_else(|| {
                source
                    .is_none()
                    .then(|| format!("{id:?} is not in the resources table"))
            });
            ResourceRef {
                slot: slot.to_string(),
                id: id.clone(),
                source,
                dark,
            }
        });
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
            consumers: self.consumers.get(&n.address).cloned().unwrap_or_default(),
            // The child's face, resolved through the same seam the loader uses — and **rebased on
            // the child**, so the child's own nested references resolve next to it exactly as they
            // do under a real load. Without that a child that itself nests describes every
            // re-exported port as dark, which would be the projection lying rather than omitting.
            // A child that will not resolve or will not describe leaves no boundary; the dark ref
            // already says why.
            boundary: resource
                .as_ref()
                .filter(|r| r.slot != "sample" && r.dark.is_none())
                .and_then(|r| r.source.as_deref())
                .and_then(|source| {
                    let id = self.resolver.canonical(source, None);
                    let text = self.resolver.resolve_text(&id).ok()?;
                    let rebased = Rebased {
                        inner: self.resolver,
                        referrer: id,
                    };
                    describe_patch(&text, self.registry, &rebased).ok()
                }),
            resource,
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
            unmatched,
        }
    }

    /// The **resources view**.
    pub fn resources(&self) -> ResourcesView {
        let mut refs: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        let mut dangling: Vec<ResourceRef> = Vec::new();
        for n in &self.doc.nodes {
            for (slot, id) in resource_refs(n) {
                if self.doc.resources.contains_key(id) {
                    refs.entry(id)
                        .or_default()
                        .push(format!("{slot}:{}", n.address));
                } else {
                    dangling.push(ResourceRef {
                        slot: slot.to_string(),
                        id: id.clone(),
                        source: None,
                        dark: Some(format!(
                            "{:?} referenced by {} is not in the resources table",
                            id, n.address
                        )),
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
                    dark: self.dark_ids.get(id).cloned(),
                })
                .collect(),
            dangling,
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

/// One node's resource references paired with their slots, in the format's own slot order — the
/// read-side twin of the format's private `resource_refs`.
fn resource_refs(n: &NodeDoc) -> Vec<(&'static str, &String)> {
    [
        ("sample", &n.sample),
        ("voice", &n.voice),
        ("patch", &n.patch),
    ]
    .into_iter()
    .filter_map(|(slot, r)| r.as_ref().map(|id| (slot, id)))
    .collect()
}

/// Split a wire-ref into `(node, port)` — the last `.` separates, because node addresses carry
/// none (the same rule the loader's own `parse_wire` uses).
fn split_wire(reference: &str) -> (&str, Option<&str>) {
    match reference.rsplit_once('.') {
        Some((node, port)) => (node, Some(port)),
        None => (reference, None),
    }
}

/// Invert the document's edges once: every node input and every output pipe's feed, keyed by the
/// **source** node. The sole-output sugar is resolved against the registry so a consumer that
/// wrote `"/kick"` still reports which port it took.
fn build_consumers(doc: &NormalizedDoc, registry: &Registry) -> BTreeMap<String, Vec<OutEdge>> {
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
        let (src, port) = split_wire(reference);
        let port = port.map(str::to_string).or_else(|| sole_output(src));
        out.entry(src.to_string())
            .or_default()
            .push(OutEdge { port, node, input });
    };
    for n in &doc.nodes {
        for (name, v) in &n.inputs {
            if let InputValue::Wire { from } = v {
                record(from, Some(n.address.clone()), name.clone());
            }
        }
    }
    for (name, e) in doc.interface.iter().flat_map(|i| i.outputs.iter()) {
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
fn render_boundary(b: &PatchBoundary) -> String {
    let ports = |ps: &[crate::introspect::PortInfo], dark: &[String]| -> String {
        let mut all: Vec<String> = ps.iter().map(|p| p.signature_fragment()).collect();
        all.extend(dark.iter().map(|d| format!("{d}:DARK")));
        all.join(", ")
    };
    format!(
        "{} ({}) -> {}",
        b.instrument,
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
        // The type predicate cuts input pipes (an output pipe declares no type of its own).
        let by_type = p.pipes(&Selection::Type("f32".into()));
        assert_eq!(by_type.inputs.len(), 1);
        assert!(by_type.outputs.is_empty());
        assert_eq!(
            p.pipes(&Selection::names(["nope"])).unmatched,
            ["nope".to_string()]
        );
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
        assert!(res.contains("voice=missing-voice DARK:"), "{res}");
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
            if let Some(r) = node.get("$ref").and_then(Value::as_str) {
                let name = r.rsplit('/').next().unwrap_or(r).to_string();
                if !active.insert(name.clone()) {
                    out.insert(format!("{path} → <recursive {name}>"));
                    return;
                }
                if let Some(def) = defs.get(&name) {
                    walk(def, defs, path, out, active);
                }
                active.remove(&name);
                return;
            }
            for key in ["anyOf", "oneOf", "allOf"] {
                if let Some(arr) = node.get(key).and_then(Value::as_array) {
                    let mut recursed = false;
                    for sub in arr {
                        if sub.get("type").and_then(Value::as_str) == Some("null") {
                            continue;
                        }
                        walk(sub, defs, path, out, active);
                        recursed = true;
                    }
                    if recursed {
                        return;
                    }
                }
            }
            if let Some(props) = node.get("properties").and_then(Value::as_object) {
                for (k, v) in props {
                    let child = if path.is_empty() {
                        k.clone()
                    } else {
                        format!("{path}.{k}")
                    };
                    walk(v, defs, &child, out, active);
                }
                return;
            }
            if let Some(ap) = node.get("additionalProperties") {
                if ap.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                    walk(ap, defs, &format!("{path}{{}}"), out, active);
                    return;
                }
            }
            if let Some(items) = node.get("items") {
                walk(items, defs, &format!("{path}[]"), out, active);
                return;
            }
            out.insert(path.to_string());
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
    }
}
