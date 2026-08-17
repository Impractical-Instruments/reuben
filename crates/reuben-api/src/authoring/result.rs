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

use alloc::string::String;
use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use reuben_document::introspect as core_introspect;
use reuben_document::{contract as core_contract, edit as core_edit};

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
