//! What the authoring verbs answer with — the window's own result shapes, and the conversions that
//! fill them from the engine's.
//!
//! Two kinds of `///` live here and only one reaches a model. **Every field doc is advertised**,
//! and so is the **type** doc of any type that appears as a nested `$defs` entry ([`Diag`],
//! [`Report`]) — schemars lifts both into the `description` a door puts in front of a model. A
//! type used only as a tool's *root* output ([`EditResult`], [`Operators`], [`DocumentView`]) has
//! its type doc dropped, so that one is for Rust readers. The distinction is not stable: making a
//! root type nest inside another promotes its doc onto the wire.
//!
//! So write every `///` here as if it ships. Advertised prose takes no rustdoc link syntax, no
//! issue numbers and no crate paths — a model can resolve none of them; notes for humans go in
//! `//` comments, which schemars does not pick up. Guarded end-to-end over the real advertised
//! surface by `advertised_prose_is_model_facing`.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use reuben_document::format::{DocValue, Layout};
use reuben_document::introspect as core_introspect;
use reuben_document::{contract as core_contract, edit as core_edit, structure as core_structure};

/// One diagnostic — an error or a warning — with the offending node/port when the loader localized
/// it, so an agent can jump straight to the offending node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Diag {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    pub message: String,
}

/// Outcome of validating (or swap-validating) an instrument document:
/// loadable + cycle-free means `ok`. Resource problems are advisory `warnings`
/// and do not flip `ok`; a `{ok: false}` report is a tool *working*, not a tool failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Report {
    pub ok: bool,
    pub errors: Vec<Diag>,
    pub warnings: Vec<Diag>,
}

/// The shape every document verb returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct EditResult {
    /// The whole-document validation report. `ok` is the write decision: `true` means the edit
    /// passed and was persisted; `false` means nothing was written and `errors` says why.
    pub report: Report,
    // The errors of a rejected edit that the document already had before it; `report.errors` then
    // holds only the rest. A `//`, not a `///`: a field doc ships once per verb's output schema, and
    // the server instructions already say what this is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_existing: Vec<Diag>,
    /// Whether the edit was actually written to the source. Equals `report.ok` — surfaced
    /// explicitly so a small model never has to infer "did my change land?" from the report shape.
    pub written: bool,
    /// The content hash of what is **now persisted** at the source: the new document on a
    /// successful write, the unchanged prior document on a rejected one. The token a later
    /// `expect`-guarded write compares — opaque, compare-only. It is the hash of the edited
    /// source alone, which is the playing instrument's only when that source is its top document.
    pub hash: String,
    /// What the edit broke, degraded, or qualified on the way — the cascade a removal unwired, the
    /// refs a rename rewrote, a caveat on where a written value applies. Empty for a clean
    /// surgical edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The rendered echo of what the verb did — the node zoom of an added node, the pipe view of a
    /// changed pipe, the index after a removal, or a value edit's from -> to line. The agent's read
    /// of the result, in the same compact grammar it reads the rest of the document through.
    pub zoom: String,
}

/// One port, flattened for agent grounding. Inputs and outputs share this shape;
/// a plan-time `Constant` is just an input with `constant: true`
/// — an immutable port set in a node's `config` block, never wired in `inputs`. Optional metadata
/// appears only where the port's type carries it: `default`/`min`/`max`/`unit`/`curve` for a swept
/// scalar, `default`/`min`/`max` for an integer, `default`/`variants` for an enum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PortInfo {
    pub name: String,
    /// The port's type as the glossary's word: `"value"` (a held `f32` Value),
    /// `"signal"` (a dense `f32_buffer` Signal), `"int"`, `"enum"`, `"message"` (Note),
    /// `"harmony"` (Harmony), `"vocab"`, or `"string"`. The two numeric kinds are one wiring
    /// family with a single implicit bridge: `value` → `signal` materializes;
    /// the reverse is a hard error.
    pub kind: String,
    /// A plan-time `Constant`: set in the node's `config` block, not wired in `inputs`.
    /// Omitted (false) for an ordinary runtime input or any output.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub constant: bool,
    /// Unwired default: a number for a scalar/integer control, the variant symbol for an enum.
    /// Omitted for a port with no settable default (audio buffers, `Note`/`Harmony`, outputs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(with = "Option<crate::schema::Literal>")]
    pub default: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "str::is_empty")]
    pub unit: String,
    /// `"linear"` or `"exponential"` for a swept scalar; omitted for non-scalar ports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<String>,
    /// The ordered enum choices; empty for non-enum ports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// Boundary-only: the logical channel a signal pipe binds — the input channel
    /// an input pipe reads, or the master channel an output pipe feeds, when the instrument is
    /// played at top level. Omitted for unbound pipes and operator ports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<usize>,
}

/// One operator's self-description, flattened from its descriptor for agent grounding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct OperatorInfo {
    pub type_name: String,
    /// The whole input surface as one list: runtime inputs first, then plan-time `Constant` ports
    /// marked `constant: true`. There is no separate `params`/`enums`/`constants` split — a port's
    /// `kind` and its optional metadata already say whether it is a scalar, integer, or enum.
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
}

/// The operator set under an object root (MCP requires one), in exactly one of the verb's two
/// projections of the same registry truth — `operators` (full port objects, the default) or
/// `signatures` (the compact mode), keyed by the `compact` argument.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Operators {
    /// Full mode: one entry per registered operator (or the single filtered one), in registry
    /// order. Absent in compact mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operators: Option<Vec<OperatorInfo>>,
    /// Compact mode: one generated signature line per operator —
    /// `name(inputs; config: constants; res: resource-slots) -> outputs`. Absent in full mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signatures: Option<Vec<String>>,
}

/// The face a host instrument wires against when it nests this one: one port per `interface`
/// name, described as if the whole document were an Operator. An input pipe is typed by its own
/// declaration; an output pipe inherits type and metadata from the internal port feeding it, then
/// takes whatever range and unit the entry overrides.
///
/// The rendered line grammar every other read answers in is what a model gets; this is the
/// structured shape, for a program building a control surface off the same question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Boundary {
    /// The document's `instrument` name.
    pub instrument: String,
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
    /// Declared boundary ports whose internal target went dark this load (an unavailable nested
    /// child) — real ports the description cannot type.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dark_inputs: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dark_outputs: Vec<String>,
    /// Non-fatal load warnings (unresolved resources and the like), advisory as in validation and
    /// localized the same way.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<Diag>,
}

impl Boundary {
    /// No boundary port of any kind — typed or dark, either direction. The instrument nests but
    /// exposes nothing to wire.
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
            && self.outputs.is_empty()
            && self.dark_inputs.is_empty()
            && self.dark_outputs.is_empty()
    }
}

/// A structural read of a document: the rendered view.
///
/// One rendered string rather than five structured shapes, for the reason the projection exists at
/// all — the compact line grammar *is* the deliverable, and four alternative payloads would put
/// schemas a caller never uses into every turn's grounding. It is the same channel a document
/// verb's `zoom` echoes through, so an agent reads a document and reads back its own edit in one
/// grammar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DocumentView {
    /// Which view this is — echoed so a caller that defaulted it knows what it got.
    pub view: String,
    /// The rendered view.
    pub text: String,
}

/// One target's answer to a wire compatibility query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WireVerdict {
    pub target: super::WireTarget,
    /// Whether the wire's port types are accepted. A wire that would close a cycle can still be
    /// `true`: that is reported when the edit is written.
    pub ok: bool,
    /// Why not, when `ok` is false.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// What `wire_compatibility` answers: one verdict per target, in the order asked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WireVerdicts {
    pub verdicts: Vec<WireVerdict>,
}

/// What `read_instrument` answers: one document as data — every node with its values, wires and
/// resolved ports, the interface pipes, the resources, and where each is drawn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InstrumentStructure {
    /// The document's `instrument` name.
    pub instrument: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The content hash of the document read: the `hash` a document verb reports for it, and the
    /// value to pass as `expect` on the next write.
    pub hash: String,
    /// Every node, in document order.
    pub nodes: Vec<StructureNode>,
    pub inputs: Vec<InterfaceInput>,
    pub outputs: Vec<InterfaceOutput>,
    pub resources: Vec<ResourceEntry>,
    /// The load report. A document that does not load is still read in full, and `errors` says
    /// where it is broken: a refused wire is an error on the node and input holding it, and a
    /// nested child that did not load is a diagnostic on the node referencing it.
    pub report: Report,
}

/// One node of a document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StructureNode {
    pub address: String,
    pub type_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Where the node is drawn; absent until a position is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Position>,
    /// The constants the node sets. One it leaves out takes the default its port reports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constants: Vec<NodeConstant>,
    /// The inputs the node sets or wires. One it leaves out takes the default its port reports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<NodeInput>,
    /// The resources the node references, by slot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<NodeResource>,
    /// The ports a wire can name on this node: the operator's own, or for a `subpatch` node its
    /// child's interface. Empty when the type is unknown or a `subpatch` child did not load.
    pub ports: NodePorts,
    /// The interface of the document this node references through `voice` or `patch`, when it
    /// loaded. For a `subpatch` node it equals `ports`. For a `voicer` node it is the voice
    /// patch's interface, which the voicer drives itself: no wire in this document can name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child: Option<NodePorts>,
}

/// A set of ports, by direction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NodePorts {
    /// Runtime inputs first, then constants marked `constant: true`.
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
}

/// A canvas position, in canvas units.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Position {
    pub x: f32,
    pub y: f32,
}

/// One constant a node sets.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NodeConstant {
    pub name: String,
    /// A number, or an enum symbol.
    #[schemars(with = "crate::schema::Literal")]
    pub value: serde_json::Value,
}

/// One input a node sets or wires.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NodeInput {
    pub name: String,
    pub value: InputValue,
}

/// What a node input holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputValue {
    /// A number, or an enum symbol.
    Literal {
        #[schemars(with = "crate::schema::Literal")]
        value: serde_json::Value,
    },
    /// The wire feeding the input.
    Wire { from: super::WireSource },
}

/// One resource reference on a node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NodeResource {
    /// `sample`, `voice` or `patch`.
    pub slot: String,
    /// The id of a `resources` entry.
    pub id: String,
}

/// One interface input pipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InterfaceInput {
    /// The pipe as a port: its name, declared type, and its effective range and curve.
    pub port: PortInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Position>,
    /// The range and curve exactly as the pipe declares them.
    #[serde(default)]
    pub declared: DeclaredMeta,
}

/// An input pipe's range and curve as written, each absent where the pipe declares none: an
/// undeclared range is effectively unbounded, and an undeclared curve is the curve of the port
/// the pipe feeds, so [`InterfaceInput::port`] cannot say which of its values were declared.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DeclaredMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// `linear` or `exponential`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<String>,
}

/// One interface output pipe, as declared. Its type is that of the port feeding it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InterfaceOutput {
    pub name: String,
    /// The port the pipe is fed from.
    pub from: super::WireSource,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Position>,
}

/// One entry of a document's resources table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ResourceEntry {
    pub id: String,
    /// The source as the document writes it.
    pub source: String,
    /// The source the document's reference resolves to: what a load of this document reads.
    pub canonical: String,
}

// --- filling the window's shapes from the engine's ------------------------------------------------
//
// `pub(crate)` associated functions rather than `From` impls: a public `impl From<reuben_core::…>`
// would put the engine's types in this crate's public API and its rustdoc, which is the leak the
// boundary exists to close. One direction only — the window is where the engine's answers stop.

impl Diag {
    pub(crate) fn from_core(d: &core_contract::Diag) -> Self {
        Diag {
            node: d.node.clone(),
            port: d.port.clone(),
            message: d.message.clone(),
        }
    }
}

impl Report {
    pub(crate) fn from_core(r: core_contract::Report) -> Self {
        Report {
            ok: r.ok,
            errors: r.errors.iter().map(Diag::from_core).collect(),
            warnings: r.warnings.iter().map(Diag::from_core).collect(),
        }
    }
}

impl EditResult {
    pub(crate) fn from_core(r: core_edit::EditResult) -> Self {
        EditResult {
            report: Report::from_core(r.report),
            pre_existing: r.pre_existing.iter().map(Diag::from_core).collect(),
            written: r.written,
            hash: r.hash,
            notes: r.notes,
            zoom: r.zoom,
        }
    }
}

impl PortInfo {
    pub(crate) fn from_core(p: &core_introspect::PortInfo) -> Self {
        PortInfo {
            name: p.name.clone(),
            kind: p.kind.clone(),
            constant: p.constant,
            default: p.default.clone(),
            min: p.min,
            max: p.max,
            unit: p.unit.clone(),
            curve: p.curve.clone(),
            variants: p.variants.clone(),
            channel: p.channel,
        }
    }
}

impl Boundary {
    pub(crate) fn from_core(b: &core_introspect::PatchBoundary) -> Self {
        Boundary {
            instrument: b.instrument.clone(),
            inputs: b.inputs.iter().map(PortInfo::from_core).collect(),
            outputs: b.outputs.iter().map(PortInfo::from_core).collect(),
            dark_inputs: b.dark_inputs.clone(),
            dark_outputs: b.dark_outputs.clone(),
            warnings: b.warnings.iter().map(Diag::from_core).collect(),
        }
    }
}

impl OperatorInfo {
    pub(crate) fn from_core(o: &core_introspect::OperatorInfo) -> Self {
        OperatorInfo {
            type_name: o.type_name.clone(),
            inputs: o.inputs.iter().map(PortInfo::from_core).collect(),
            outputs: o.outputs.iter().map(PortInfo::from_core).collect(),
            resources: o.resources.clone(),
        }
    }
}

impl NodePorts {
    fn from_core(p: &core_structure::Ports) -> Self {
        NodePorts {
            inputs: p.inputs.iter().map(PortInfo::from_core).collect(),
            outputs: p.outputs.iter().map(PortInfo::from_core).collect(),
        }
    }
}

impl Position {
    fn from_core(l: Layout) -> Self {
        Position { x: l.x, y: l.y }
    }
}

fn literal(v: &DocValue) -> serde_json::Value {
    match v {
        DocValue::Number(n) => serde_json::json!(n),
        DocValue::Symbol(s) => serde_json::json!(s),
    }
}

fn wire_source(s: &core_structure::Source) -> super::WireSource {
    match s {
        // The sugar on a source with no single output names no port; the report refuses that wire.
        core_structure::Source::Output { node, port } => super::WireSource::NodeOutput {
            node: node.clone(),
            port: port.clone().unwrap_or_default(),
        },
        core_structure::Source::InterfaceInput(name) => {
            super::WireSource::InterfaceInput { name: name.clone() }
        }
    }
}

impl InstrumentStructure {
    pub(crate) fn from_core(s: core_structure::Structure) -> Self {
        InstrumentStructure {
            instrument: s.instrument,
            description: s.description,
            hash: s.hash,
            nodes: s.nodes.iter().map(StructureNode::from_core).collect(),
            inputs: s
                .inputs
                .iter()
                .map(|p| InterfaceInput {
                    port: PortInfo::from_core(&p.port),
                    layout: p.layout.map(Position::from_core),
                    declared: DeclaredMeta {
                        min: p.declared.min,
                        max: p.declared.max,
                        curve: p.declared.curve.map(str::to_string),
                    },
                })
                .collect(),
            outputs: s
                .outputs
                .iter()
                .map(|p| InterfaceOutput {
                    name: p.name.clone(),
                    from: wire_source(&p.from),
                    channel: p.channel,
                    unit: p.unit.clone(),
                    min: p.min,
                    max: p.max,
                    layout: p.layout.map(Position::from_core),
                })
                .collect(),
            resources: s
                .resources
                .into_iter()
                .map(|r| ResourceEntry {
                    id: r.id,
                    source: r.source,
                    canonical: r.canonical,
                })
                .collect(),
            report: Report::from_core(s.report),
        }
    }
}

impl StructureNode {
    fn from_core(n: &core_structure::Node) -> Self {
        StructureNode {
            address: n.address.clone(),
            type_name: n.type_name.clone(),
            description: n.description.clone(),
            layout: n.layout.map(Position::from_core),
            constants: n
                .constants
                .iter()
                .map(|(name, value)| NodeConstant {
                    name: name.clone(),
                    value: literal(value),
                })
                .collect(),
            inputs: n
                .inputs
                .iter()
                .map(|(name, input)| NodeInput {
                    name: name.clone(),
                    value: match input {
                        core_structure::Input::Literal(value) => InputValue::Literal {
                            value: literal(value),
                        },
                        core_structure::Input::Wire(from) => InputValue::Wire {
                            from: wire_source(from),
                        },
                    },
                })
                .collect(),
            resources: n
                .resources
                .iter()
                .map(|(slot, id)| NodeResource {
                    slot: slot.to_string(),
                    id: id.clone(),
                })
                .collect(),
            ports: NodePorts::from_core(&n.ports),
            child: n.child.as_ref().map(NodePorts::from_core),
        }
    }
}
