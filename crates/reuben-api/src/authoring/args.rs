//! The argument surface: one flat struct per verb.
//!
//! Flat because that is what MCP and generated wrappers want; the engine behind the window stays
//! free to be shaped however suits it. Every `///` here is advertised prose — schemars lifts it
//! into the field `description` a model reads — so it stays about what to pass and never about how
//! the window is built.
//!
//! Each mutating verb carries an optional `expect` content-hash write guard. It is an ordinary
//! field of a type the window owns, applied once in [`verbs`](super::verbs) rather than
//! re-implemented per door.
//!
//! Every struct here is `deny_unknown_fields`, which schemars carries onto the wire as
//! `additionalProperties: false`.

use alloc::borrow::Cow;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::surface::{SurfaceControl, SurfaceKind};

/// Arguments for `describe_operators`: an optional `name` filter plus the `compact` mode switch.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct DescribeBoundary {
    /// The instrument document (a path for this door).
    pub source: String,
}

/// Arguments for `validate_instrument`: the document to validate, named by its opaque `source`.
///
// There is no inline `document` arm: a model that can hand the verb a whole document is a model
// that had to hold one. `source` is opaque and door-resolved (a path natively), exactly as it is on
// every document verb.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ValidateInstrument {
    /// The instrument document (a path for this door). Nested references resolve sibling-first
    /// from its directory, then the library root.
    pub source: String,
}

// --- document verbs -------------------------------------------------------------------------------

/// Arguments for `new_instrument`: where to create the document, and its name.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NewInstrument {
    /// Where to create the document (a path for this door). Refuses to overwrite an existing one.
    pub source: String,
    /// The `instrument` name for the new document.
    pub name: String,
}

/// Arguments for `copy_instrument`: the document to copy, and where the copy goes.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CopyInstrument {
    /// The document to copy. It is not changed.
    pub from: String,
    /// Where to write the copy: a new source in the same directory as `from`. Refuses to overwrite
    /// an existing document. It is resolved as the store resolves any top-level source, so with a
    /// store scoped to `from` a bare name lands beside it and a path relative to the working
    /// directory does not.
    pub to: String,
}

/// Arguments for `set_instrument_name`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct AddInstrumentNode {
    /// The document to edit.
    pub source: String,
    /// The node's OSC address (e.g. `/osc`), unique within the instrument.
    pub address: String,
    /// The operator type (must be registered, e.g. `oscillator`).
    #[serde(rename = "type")]
    pub type_name: String,
    /// Inputs by name: a literal (number, enum symbol, or `str` text) or a wire-ref
    /// `{"from": "/node.port"}`.
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, crate::schema::NodeInput>")]
    pub inputs: BTreeMap<String, serde_json::Value>,
    /// Instantiate-time constants by name (plan-time `config`, never wired).
    #[serde(default)]
    #[schemars(with = "BTreeMap<String, crate::schema::Literal>")]
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
    /// Optional resource bindings (subpatch only): the child's resource id -> a resource id in
    /// this document, e.g. {"sample": "kick2"}. The child plays this document's source for that id
    /// instead of its own default, so one child is reused with a different sample per use. Every
    /// key must be an id in the child's own resources.
    #[serde(default)]
    pub bind: BTreeMap<String, String>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `remove_instrument_node` — cascades: unwires every consumer, reports what it broke.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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

/// Arguments for `set_instrument_input`: set a node input, or an interface input pipe's seed, to a
/// **literal** value.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetInstrumentInput {
    /// The document to edit.
    pub source: String,
    /// The address of the node.
    pub address: String,
    /// The input name.
    pub input: String,
    /// The literal value: a number, an enum symbol, or a `str` input's text. (Wiring is
    /// `wire_instrument_input`.)
    #[schemars(with = "crate::schema::Literal")]
    pub value: serde_json::Value,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_inputs_by_intent`: apply one curated intent word as a batch of
/// value edits.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetInstrumentInputsByIntent {
    /// The document to edit.
    pub source: String,
    /// The intent word, exactly as the intent vocabulary spells it (`warmer`, `looser`,
    /// `more consonant`).
    pub word: String,
    /// Which reading of an overloaded word to take: `timbral`, `rhythmic`, or `tonal`. Omit unless
    /// the word has more than one row; the report names the reading it passed over.
    #[serde(default)]
    pub section: Option<String>,
    /// Narrow the edit to these node addresses. Omit to move every node the word's moves match. A
    /// term matching nothing is reported back, never silently dropped.
    #[serde(default)]
    pub target: Vec<String>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `wire_instrument_input`: wire a node input from a source port.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct SetInstrumentConstant {
    /// The document to edit.
    pub source: String,
    /// The address of the node.
    pub address: String,
    /// The constant name.
    pub name: String,
    /// The constant value: a number, or a symbol string.
    #[schemars(with = "crate::schema::Literal")]
    pub value: serde_json::Value,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `remove_instrument_constant`: drop a node's constant back to the operator's
/// default.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveInstrumentConstant {
    /// The document to edit.
    pub source: String,
    /// The address of the node.
    pub address: String,
    /// The constant name to remove.
    pub name: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `add_instrument_interface_input`: add a boundary input pipe.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AddInstrumentInterfaceInput {
    /// The document to edit.
    pub source: String,
    /// The pipe name (mints an address `/name` internal nodes consume from).
    pub name: String,
    /// The declared `Arg` type: `f32_buffer`, `f32`, `i32`, `str`, `note`, `harmony`, `pitch`, or
    /// an enum type name.
    #[serde(rename = "type")]
    pub type_name: String,
    /// Optional logical input channel (signal pipes only).
    #[serde(default)]
    pub channel: Option<usize>,
    /// The pipe's value when nothing feeds it: a number, an enum symbol, or a `str` pipe's text.
    /// Omit for a bare signal pipe, which renders silence unfed. Later changed with
    /// `set_instrument_input`.
    #[serde(default)]
    #[schemars(with = "Option<crate::schema::Literal>")]
    pub value: Option<serde_json::Value>,
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
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct RemoveInstrumentInterfacePipe {
    /// The document to edit.
    pub source: String,
    /// The pipe name to remove.
    pub name: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_interface_input_meta`: update an input pipe's quantity contract
/// (each provided field is written; omitted fields are unchanged). The pipe's value is
/// `set_instrument_input`'s, addressed `/name` with input `in`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetInstrumentInterfaceInputMeta {
    /// The document to edit.
    pub source: String,
    /// The input pipe name.
    pub name: String,
    #[serde(default)]
    pub channel: Option<usize>,
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
#[serde(deny_unknown_fields)]
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

/// Which half of the interface a pipe is declared in: input and output pipe names are separate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PipeDirection {
    Input,
    Output,
}

/// A pipe metadata field the set-meta verbs write. `curve` is an input pipe's only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetaField {
    Channel,
    Min,
    Max,
    Curve,
    Unit,
}

/// Arguments for `clear_instrument_interface_meta`: remove metadata fields from a pipe.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClearInstrumentInterfaceMeta {
    /// The document to edit.
    pub source: String,
    /// The pipe name.
    pub name: String,
    /// Whether `name` is an input or an output pipe.
    pub direction: PipeDirection,
    /// The fields to remove.
    #[schemars(length(min = 1))]
    pub fields: Vec<MetaField>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `wire_instrument_interface_output`: feed an existing output pipe from another
/// internal port.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WireInstrumentInterfaceOutput {
    /// The document to edit.
    pub source: String,
    /// The output pipe name.
    pub name: String,
    /// The internal port to feed it from: `/node.port` (or `/node` sole-output sugar).
    pub from: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `add_instrument_resource`: add an id→source entry to the resources table.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
pub struct RemoveInstrumentResource {
    /// The document to edit.
    pub source: String,
    /// The resource id to remove.
    pub id: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_resource`: point an existing resource id at a new source.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetInstrumentResource {
    /// The document to edit.
    pub source: String,
    /// The resource id to repoint. It must already be in the table.
    pub id: String,
    /// The new resource source, relative to the document as `add_instrument_resource`'s is.
    pub resource_source: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_node_resource`: the resource id a node reads through one slot.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetInstrumentNodeResource {
    /// The document to edit.
    pub source: String,
    /// The node's address.
    pub address: String,
    /// `sample`, `voice` or `patch`.
    pub slot: String,
    /// An id in the document's `resources` table.
    pub id: String,
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `set_instrument_layout`: canvas positions for many nodes and interface pipes,
/// written together. An unknown target, or one named twice, refuses the whole call.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetInstrumentLayout {
    /// The document to edit.
    pub source: String,
    /// One position per target.
    pub positions: Vec<LayoutPosition>,
    #[serde(default)]
    pub expect: Option<String>,
}

/// One target's canvas position, in canvas units.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutPosition {
    pub target: LayoutTarget,
    pub x: f32,
    pub y: f32,
}

/// What a position is set on: a node address string (`/osc`, or `/<name>` for an interface input
/// pipe), or an interface output pipe as `{ "output": name }`. Output pipes are their own
/// namespace, so the tag is what tells `main` the output from `/main` the input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, schemars::JsonSchema)]
#[serde(untagged)]
pub enum LayoutTarget {
    Node(String),
    Output(OutputTarget),
}

impl<'de> Deserialize<'de> for LayoutTarget {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // By shape, by hand: derived `untagged` would also take serde's `["main"]` sequence form of
        // `OutputTarget`, and would bury a misspelled key under "did not match any variant". The
        // map goes straight to the derived impl rather than through a buffered `Value`, which
        // would keep only the last of two duplicate keys and hide the duplicate.
        struct Shape;

        impl<'de> serde::de::Visitor<'de> for Shape {
            type Value = LayoutTarget;

            fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str("a node address string or `{\"output\": name}`")
            }

            fn visit_str<E: serde::de::Error>(self, address: &str) -> Result<LayoutTarget, E> {
                Ok(LayoutTarget::Node(address.into()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<LayoutTarget, A::Error> {
                OutputTarget::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(LayoutTarget::Output)
            }
        }

        deserializer.deserialize_any(Shape)
    }
}

/// An interface output pipe, named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OutputTarget {
    pub output: String,
}

// --- restore --------------------------------------------------------------------------------------

/// Arguments for `restore_instrument`: put a document back to an earlier state of itself, named by
/// the content hash an edit reported for it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreInstrument {
    /// The document to restore.
    pub source: String,
    /// The content hash of the state to return to.
    pub to: String,
    /// The content hash the document must have now. Required: a whole-document overwrite is never
    /// unguarded.
    pub expect: String,
}

// --- editor queries -------------------------------------------------------------------------------

/// The end a wire leaves from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireSource {
    /// A node's output port. On a `subpatch` node, `port` is one of its child's interface output
    /// names.
    NodeOutput { node: String, port: String },
    /// One of the document's own interface input pipes, by name.
    InterfaceInput { name: String },
}

/// The end a wire lands on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireTarget {
    /// A node's input port. On a `subpatch` node, `port` is one of its child's interface input
    /// names.
    NodeInput { node: String, port: String },
    /// One of the document's own interface output pipes, by name: the wire becomes its `from`.
    InterfaceOutput { name: String },
}

/// Arguments for `wire_compatibility`: one source, and the targets to judge it against.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WireCompatibility {
    /// The instrument document (a path for this door).
    pub source: String,
    /// Where the wire leaves from.
    pub from: WireSource,
    /// The targets to judge, in the order to answer them; omit for every target the document has.
    #[serde(default)]
    pub to: Option<Vec<WireTarget>>,
}

/// Arguments for `read_instrument`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadInstrument {
    /// The instrument document (a path for this door).
    pub source: String,
}

/// Arguments for `read_instrument_tree`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReadInstrumentTree {
    /// The top instrument document of the tree.
    pub source: String,
}

// --- surface docs ---------------------------------------------------------------------------------

/// Arguments for `describe_instrument_surface`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeInstrumentSurface {
    /// The instrument whose surface doc to read (a path for this door).
    pub source: String,
    /// `midi` for the instrument's MIDI map, `screen` for its on-screen surface doc.
    pub surface: SurfaceKind,
}

/// Arguments for `set_instrument_surface_control`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetInstrumentSurfaceControl {
    /// The instrument whose surface doc to edit (a path for this door).
    pub source: String,
    /// `midi` for the instrument's MIDI map, `screen` for its on-screen surface doc.
    pub surface: SurfaceKind,
    /// The position in `controls` to replace; omit to append.
    #[serde(default)]
    pub index: Option<usize>,
    /// The control to write. A MIDI map reads `bind`, `midi`, `min` and `max`; a screen doc reads
    /// `bind`, `label`, `widget`, `group`, `min`, `max`, `note`, `velocity` and `degree`.
    #[serde(deserialize_with = "closed_control")]
    #[schemars(with = "ClosedControl")]
    pub control: SurfaceControl,
    /// Optional content-hash write guard: the doc's `hash` as last read. A mismatch rejects the
    /// write.
    #[serde(default)]
    pub expect: Option<String>,
}

/// The advertised shape of a `control` argument: a surface doc's control, closed.
struct ClosedControl;

impl schemars::JsonSchema for ClosedControl {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "ClosedSurfaceControl".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = SurfaceControl::json_schema(generator);
        schema.insert("additionalProperties".into(), false.into());
        schema
    }
}

/// A `control` argument, refusing a key [`SurfaceControl`] does not declare. The declared keys are
/// read from its schema, so the two cannot drift. [`SurfaceDoc::parse`](crate::surface::SurfaceDoc)
/// stays lenient, because a hand-written doc may carry keys another reader of the format uses.
fn closed_control<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<SurfaceControl, D::Error> {
    use schemars::JsonSchema;
    use serde::de::Error;

    let value = serde_json::Value::deserialize(deserializer)?;
    let schema = ClosedControl::json_schema(&mut schemars::SchemaGenerator::default());
    let declared = schema
        .get("properties")
        .and_then(serde_json::Value::as_object);
    if let (Some(given), Some(declared)) = (value.as_object(), declared) {
        if let Some(key) = given.keys().find(|key| !declared.contains_key(*key)) {
            let expected: Vec<&str> = declared.keys().map(String::as_str).collect();
            return Err(D::Error::custom(format!(
                "unknown field `{key}`, expected one of `{}`",
                expected.join("`, `")
            )));
        }
    }
    serde_json::from_value(value).map_err(D::Error::custom)
}

/// Arguments for `remove_instrument_surface_control`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RemoveInstrumentSurfaceControl {
    /// The instrument whose surface doc to edit (a path for this door).
    pub source: String,
    /// `midi` for the instrument's MIDI map, `screen` for its on-screen surface doc.
    pub surface: SurfaceKind,
    /// The position in `controls` to remove.
    pub index: usize,
    /// Optional content-hash write guard: the doc's `hash` as last read. A mismatch rejects the
    /// write.
    #[serde(default)]
    pub expect: Option<String>,
}

/// Arguments for `move_instrument_surface_control`.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MoveInstrumentSurfaceControl {
    /// The instrument whose surface doc to edit (a path for this door).
    pub source: String,
    /// `midi` for the instrument's MIDI map, `screen` for its on-screen surface doc.
    pub surface: SurfaceKind,
    /// The position in `controls` of the control to move.
    pub from: usize,
    /// The position in `controls` it ends up at.
    pub to: usize,
    /// Optional content-hash write guard: the doc's `hash` as last read. A mismatch rejects the
    /// write.
    #[serde(default)]
    pub expect: Option<String>,
}

#[cfg(test)]
mod tests {
    use alloc::string::ToString;

    use super::*;
    use serde_json::json;

    /// An argument this verb does not declare is an error, not a dropped key. Serde's default is to
    /// drop, which would answer a caller written against a slot that has moved with a clean
    /// `written: true` for an edit that did not happen.
    #[test]
    fn a_key_the_verb_does_not_declare_is_refused() {
        // The exact regression: `default` moved off the meta verb onto the value verb, and every
        // caller written against the old surface still sends it.
        let stale = json!({ "source": "i.json", "name": "cutoff", "default": 123.0, "unit": "Hz" });
        let err = serde_json::from_value::<SetInstrumentInterfaceInputMeta>(stale)
            .expect_err("the meta verb no longer writes a value");
        assert!(
            err.to_string().contains("default"),
            "the refusal names the key it will not take: {err}"
        );

        // And the other half: a word learned on one verb, tried on the verb next door.
        let guessed = json!({ "source": "i.json", "name": "cutoff", "type": "f32",
                              "default": 880.0 });
        assert!(
            serde_json::from_value::<AddInstrumentInterfaceInput>(guessed).is_err(),
            "this verb spells the pipe's seed `value` like every other value-writing verb"
        );
        let spelled = json!({ "source": "i.json", "name": "cutoff", "type": "f32",
                              "value": 880.0 });
        let args: AddInstrumentInterfaceInput =
            serde_json::from_value(spelled).expect("the declared spelling parses");
        assert_eq!(args.value, Some(json!(880.0)));
    }

    /// A target is a node address string or an output tagged as one; nothing else parses, so a
    /// guessed `{ "input": … }` fails loudly instead of landing somewhere unintended.
    #[test]
    fn a_layout_target_is_an_address_or_a_tagged_output() {
        let parse = |target: serde_json::Value| {
            serde_json::from_value::<LayoutPosition>(
                json!({ "target": target, "x": 0.0, "y": 0.0 }),
            )
        };
        assert_eq!(
            parse(json!("/osc")).expect("an address").target,
            LayoutTarget::Node("/osc".to_string())
        );
        assert_eq!(
            parse(json!({ "output": "main" }))
                .expect("a tagged output")
                .target,
            LayoutTarget::Output(OutputTarget {
                output: "main".to_string()
            })
        );
        for bad in [
            json!({ "input": "main" }),
            json!({ "output": "main", "node": "/osc" }),
            json!(["main"]),
        ] {
            assert!(parse(bad.clone()).is_err(), "{bad} is not a target");
        }
        assert!(
            serde_json::from_value::<LayoutPosition>(json!({ "target": "/osc", "x": 0.0 }))
                .is_err(),
            "both coordinates are required"
        );
        // Text, not `json!`: a `Value` has already kept only the last of two duplicate keys.
        let twice = r#"{ "target": { "output": "a", "output": "b" }, "x": 0.0, "y": 0.0 }"#;
        let err = serde_json::from_str::<LayoutPosition>(twice).expect_err("a repeated key");
        assert!(
            err.to_string().contains("duplicate field `output`"),
            "{err}"
        );
    }

    /// The closure is not confined to the verb that lost a parameter: every argument surface the
    /// window declares is closed, so the next one to move a slot cannot fail silently either.
    #[test]
    fn every_authoring_argument_surface_is_closed() {
        // One representative per shape — a read, a create, a point-edit, a removal.
        assert!(serde_json::from_value::<DescribeInstrument>(
            json!({ "source": "i.json", "compact": true })
        )
        .is_err());
        assert!(serde_json::from_value::<NewInstrument>(
            json!({ "source": "i.json", "instrument": "x" })
        )
        .is_err());
        assert!(serde_json::from_value::<SetInstrumentInput>(
            json!({ "source": "i.json", "address": "/osc", "input": "freq", "to": 440.0 })
        )
        .is_err());
        assert!(serde_json::from_value::<RemoveInstrumentResource>(
            json!({ "source": "i.json", "id": "kick", "cascade": true })
        )
        .is_err());
        // A nested argument object is closed too, though the doc it is written into is read
        // leniently.
        let err = serde_json::from_value::<SetInstrumentSurfaceControl>(json!({
            "source": "i.json", "surface": "screen",
            "control": { "bind": "cutoff", "widgte": "radial" }
        }))
        .expect_err("a misspelled control key");
        assert!(err.to_string().contains("`widgte`"), "{err}");
        let every_key = json!({ "source": "i.json", "surface": "midi", "control": {
            "bind": "cutoff", "label": "Cut", "widget": "fader", "group": "g", "min": 0.0,
            "max": 1.0, "note": 60, "velocity": 1.0, "degree": 1, "midi": "/midi/1/cc/74"
        }});
        assert!(serde_json::from_value::<SetInstrumentSurfaceControl>(every_key).is_ok());
    }

    #[test]
    fn the_advertised_control_is_closed() {
        let schema = serde_json::to_value(schemars::schema_for!(SetInstrumentSurfaceControl))
            .expect("schema");
        let control = &schema["properties"]["control"];
        assert_eq!(control["additionalProperties"], json!(false), "{control}");
        assert!(control["properties"]["bind"].is_object(), "{control}");
    }
}
