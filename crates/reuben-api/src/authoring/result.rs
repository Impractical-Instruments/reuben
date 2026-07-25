//! What the authoring verbs answer with — the window's own result shapes, and the conversions that
//! fill them from the engine's.
//!
//! Every `///` in this file is advertised prose: schemars lifts it into the `description` a door
//! puts in front of a model. Keep it to what the field *means* and what to do with it; notes for
//! humans go in `//` comments, which schemars does not pick up.
//!
//! see rules: agent-mcp

use serde::{Deserialize, Serialize};

use reuben_core::introspect as core_introspect;
use reuben_core::{contract as core_contract, edit as core_edit};

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
    /// Whether the edit was actually written to the source. Equals `report.ok` — surfaced
    /// explicitly so a small model never has to infer "did my change land?" from the report shape.
    pub written: bool,
    /// The content hash of what is **now persisted** at the source: the new document on a
    /// successful write, the unchanged prior document on a rejected one. The token a later
    /// `expect`-guarded write compares — opaque, compare-only.
    pub hash: String,
    /// What the edit broke or degraded on the way — the cascade a `remove_instrument_node`
    /// unwired, the refs a `rename_instrument_node` rewrote. Empty for a clean surgical edit.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<String>,
    /// The rendered projection of what the verb touched — the node zoom of
    /// an added node, the pipe view of a changed pipe, the index after a removal. The agent's read
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

/// A structural read of a document: the rendered view.
///
/// One rendered string rather than five structured shapes, for the reason the projection exists at
/// all — the compact line grammar *is* the deliverable, and four alternative payloads would put
/// schemas a caller never uses into every turn's grounding. It is the same channel
/// [`EditResult::zoom`] echoes through, so an agent reads a document and reads back its own edit in
/// one grammar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DocumentView {
    /// Which view this is — echoed so a caller that defaulted it knows what it got.
    pub view: String,
    /// The rendered view.
    pub text: String,
}

// --- filling the window's shapes from the engine's ------------------------------------------------
//
// One direction only: nothing here parses a window shape back into an engine one, because the
// window is where the engine's answers stop. Compiler-checked for types and not for meaning — two
// fields can agree on shape and disagree on what they mean while both compile, which is the
// residue the boundary is bought with. see rules: agent-mcp

impl From<&core_contract::Diag> for Diag {
    fn from(d: &core_contract::Diag) -> Self {
        Diag {
            node: d.node.clone(),
            port: d.port.clone(),
            message: d.message.clone(),
        }
    }
}

impl From<core_contract::Report> for Report {
    fn from(r: core_contract::Report) -> Self {
        Report {
            ok: r.ok,
            errors: r.errors.iter().map(Diag::from).collect(),
            warnings: r.warnings.iter().map(Diag::from).collect(),
        }
    }
}

impl From<core_edit::EditResult> for EditResult {
    fn from(r: core_edit::EditResult) -> Self {
        EditResult {
            report: r.report.into(),
            written: r.written,
            hash: r.hash,
            notes: r.notes,
            zoom: r.zoom,
        }
    }
}

impl From<&core_introspect::PortInfo> for PortInfo {
    fn from(p: &core_introspect::PortInfo) -> Self {
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

impl From<&core_introspect::OperatorInfo> for OperatorInfo {
    fn from(o: &core_introspect::OperatorInfo) -> Self {
        OperatorInfo {
            type_name: o.type_name.clone(),
            inputs: o.inputs.iter().map(PortInfo::from).collect(),
            outputs: o.outputs.iter().map(PortInfo::from).collect(),
            resources: o.resources.clone(),
        }
    }
}
