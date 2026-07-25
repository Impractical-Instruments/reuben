//! The argument surface: one flat struct per verb.
//!
//! Flat because that is what MCP and generated wrappers want; the engine behind the window stays
//! free to be shaped however suits it. Every `///` here is advertised prose — schemars lifts it
//! into the field `description` a model reads — so it stays about what to pass and never about how
//! the window is built.
//!
//! Each mutating verb carries an optional `expect` content-hash write guard. It is an ordinary
//! field of a type the window owns, applied once in [`verbs`](super::verbs) rather than
//! re-implemented per door. see rules: agent-mcp

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Arguments for `describe_operators`: an optional `name` filter plus the `compact` mode switch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DescribeOperators {
    /// Restrict to one operator type; omit to list every registered operator.
    #[serde(default)]
    pub name: Option<String>,
    /// Compact mode: one generated signature line per operator instead of full
    /// port objects — the same registry truth, projected for grounding budgets. Default false.
    #[serde(default)]
    pub compact: bool,
}

/// Which view of a document `describe_instrument` cuts. The four projection views, plus the
/// host-facing boundary — the one question the projection does not answer, because it needs the
/// *resolved* face of nested children rather than what this document declares.
#[derive(
    Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum InstrumentView {
    /// Every node, one `address type` line each — the cheapest whole-document read, and the default.
    #[default]
    Index,
    /// Node zoom: literal input values and wire sources, config, description, resource ref, nested
    /// boundary, and the consumers reading each node.
    Nodes,
    /// The `interface` pipes this document declares, with ranges, curves and metadata.
    Pipes,
    /// The resources table and which node uses each entry through which slot.
    Resources,
    /// The boundary a *host instrument* wires against when nesting this one.
    Boundary,
}

impl InstrumentView {
    /// The view's wire spelling, echoed back in [`DocumentView::view`](super::DocumentView).
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            InstrumentView::Index => "index",
            InstrumentView::Nodes => "nodes",
            InstrumentView::Pipes => "pipes",
            InstrumentView::Resources => "resources",
            InstrumentView::Boundary => "boundary",
        }
    }
}

/// Arguments for `describe_instrument`: a `source`, a view, and the projection's one selection
/// grammar.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DescribeInstrument {
    /// The instrument document (a path for this door).
    pub source: String,
    /// Which view to cut; omit for the node index.
    #[serde(default)]
    pub view: InstrumentView,
    /// Narrow `nodes`/`pipes` to these node addresses or pipe names (`/` is the document header).
    /// A term matching nothing is reported back, never silently dropped.
    #[serde(default)]
    pub select: Vec<String>,
    /// Narrow `nodes`/`pipes` by declared type instead of by name. Pass this **or** `select`,
    /// never both.
    #[serde(default, rename = "type")]
    pub type_name: Option<String>,
}

/// Arguments for `describe_boundary`: the document whose nesting face to read.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DescribeBoundary {
    /// The instrument document (a path for this door).
    pub source: String,
}

/// Arguments for `validate_instrument`: the document to validate, named by its opaque `source`.
///
// There is no inline `document` arm: a model that can hand the verb a whole document is a model
// that had to hold one. `source` is opaque and door-resolved (a path natively), exactly as it is on
// every document verb. see rules: agent-mcp
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ValidateInstrument {
    /// The instrument document (a path for this door). Nested references resolve sibling-first
    /// from its directory, then the library root.
    pub source: String,
}

// --- document verbs -------------------------------------------------------------------------------

/// Arguments for `new_instrument`: where to create the document, and its name.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct NewInstrument {
    /// Where to create the document (a path for this door). Refuses to overwrite an existing one.
    pub source: String,
    /// The `instrument` name for the new document.
    pub name: String,
}

/// Arguments for `set_instrument_name`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetInstrumentName {
    /// The document to edit.
    pub source: String,
    /// The new instrument name.
    pub name: String,
    /// Optional content-hash write guard: a mismatch rejects the write.
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_description`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetInstrumentDescription {
    /// The document to edit.
    pub source: String,
    /// The description; omit to clear it.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `add_instrument_node` — the one-shot, zoom-mirroring add: the node lands fully
/// formed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddInstrumentNode {
    /// The document to edit.
    pub source: String,
    /// The node's OSC address (e.g. `/osc`), unique within the instrument.
    pub address: String,
    /// The operator type (must be registered, e.g. `oscillator`).
    #[serde(rename = "type")]
    pub type_name: String,
    /// Inputs by name: a literal (number or enum symbol) or a wire-ref `{"from": "/node.port"}`.
    #[serde(default)]
    pub inputs: BTreeMap<String, serde_json::Value>,
    /// Instantiate-time constants by name (plan-time `config`, never wired).
    #[serde(default)]
    pub config: BTreeMap<String, serde_json::Value>,
    /// Optional node description.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional `sample` resource id (sample-player operators only).
    #[serde(default)]
    pub sample: Option<String>,
    /// Optional `voice` resource id (Voicer only).
    #[serde(default)]
    pub voice: Option<String>,
    /// Optional `patch` resource id (subpatch only).
    #[serde(default)]
    pub patch: Option<String>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `remove_instrument_node` — cascades: unwires every consumer, reports what it broke.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RemoveInstrumentNode {
    /// The document to edit.
    pub source: String,
    /// The address of the node to remove.
    pub address: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `rename_instrument_node` — rewires every consumer to the new address.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RenameInstrumentNode {
    /// The document to edit.
    pub source: String,
    /// The node's current address.
    pub from: String,
    /// The node's new address (must be free).
    pub to: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_node_description`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetInstrumentNodeDescription {
    /// The document to edit.
    pub source: String,
    /// The address of the node.
    pub address: String,
    /// The description; omit to clear it.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_input`: set a node input to a **literal** value.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetInstrumentInput {
    /// The document to edit.
    pub source: String,
    /// The address of the node.
    pub address: String,
    /// The input name.
    pub input: String,
    /// The literal value: a number, or an enum symbol string. (Wiring is `wire_instrument_input`.)
    pub value: serde_json::Value,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `wire_instrument_input`: wire a node input from a source port.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WireInstrumentInput {
    /// The document to edit.
    pub source: String,
    /// The address of the consuming node.
    pub address: String,
    /// The input name.
    pub input: String,
    /// The source wire-ref: `/node.port`, or `/node` for a sole-output source.
    pub from: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `unwire_instrument_input`: revert a node input to its descriptor default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UnwireInstrumentInput {
    /// The document to edit.
    pub source: String,
    /// The address of the node.
    pub address: String,
    /// The input name to clear.
    pub input: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_constant`: set an instantiate-time constant on a node.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetInstrumentConstant {
    /// The document to edit.
    pub source: String,
    /// The address of the node.
    pub address: String,
    /// The constant name.
    pub name: String,
    /// The constant value: a number, or a symbol string.
    pub value: serde_json::Value,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `add_instrument_interface_input`: add a boundary input pipe.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddInstrumentInterfaceInput {
    /// The document to edit.
    pub source: String,
    /// The pipe name (mints an address `/name` internal nodes consume from).
    pub name: String,
    /// The declared `Arg` type: `f32_buffer`, `f32`, `i32`, `note`, `harmony`, `pitch`, or an enum
    /// type name.
    #[serde(rename = "type")]
    pub type_name: String,
    /// Optional logical input channel (signal pipes only).
    #[serde(default)]
    pub channel: Option<usize>,
    /// Optional unwired/seed value: a number, or an enum symbol string.
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    /// Optional engine-enforced range floor.
    #[serde(default)]
    pub min: Option<f64>,
    /// Optional engine-enforced range ceiling.
    #[serde(default)]
    pub max: Option<f64>,
    /// Optional sweep curve: `lin` or `exp`.
    #[serde(default)]
    pub curve: Option<String>,
    /// Optional display unit (e.g. `Hz`).
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `add_instrument_interface_output`: add a master-tap output pipe.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddInstrumentInterfaceOutput {
    /// The document to edit.
    pub source: String,
    /// The pipe name.
    pub name: String,
    /// The internal port feeding this output: `/node.port` (or `/node` sole-output sugar).
    pub from: String,
    /// Optional master output channel (omitted = broadcast).
    #[serde(default)]
    pub channel: Option<usize>,
    /// Optional presentational range floor.
    #[serde(default)]
    pub min: Option<f64>,
    /// Optional presentational range ceiling.
    #[serde(default)]
    pub max: Option<f64>,
    /// Optional display unit.
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `remove_instrument_interface_input` / `remove_instrument_interface_output`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RemoveInstrumentInterfacePipe {
    /// The document to edit.
    pub source: String,
    /// The pipe name to remove.
    pub name: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_interface_input_meta`: update an input pipe's metadata (each
/// provided field is written; omitted fields are unchanged).
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetInstrumentInterfaceInputMeta {
    /// The document to edit.
    pub source: String,
    /// The input pipe name.
    pub name: String,
    #[serde(default)]
    pub channel: Option<usize>,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    /// Sweep curve: `lin` or `exp`.
    #[serde(default)]
    pub curve: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_interface_output_meta`: update an output pipe's metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SetInstrumentInterfaceOutputMeta {
    /// The document to edit.
    pub source: String,
    /// The output pipe name.
    pub name: String,
    #[serde(default)]
    pub channel: Option<usize>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `add_instrument_resource`: add an id→source entry to the resources table.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AddInstrumentResource {
    /// The document to edit.
    pub source: String,
    /// The logical resource id (what a node's `sample`/`voice`/`patch` references).
    pub id: String,
    /// The resource source (a file path for this door).
    pub resource_source: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `remove_instrument_resource`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RemoveInstrumentResource {
    /// The document to edit.
    pub source: String,
    /// The resource id to remove.
    pub id: String,
    #[serde(default)]
    pub expect: Option<String>,
}
