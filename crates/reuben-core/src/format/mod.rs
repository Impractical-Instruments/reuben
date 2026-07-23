//! Instrument format — the JSON canonical document (**v2**, **v3**).
//!
//! An instrument is plain data: a list of operator `nodes`, each carrying one `inputs` map
//! and an optional `config` block, plus an `interface` block of named boundary
//! **pipes**. A node's `inputs` entry is either a **literal** (a number, or an
//! `Enum` symbol like `"Hp"`) or a **wire-ref** to another node's output
//! (`{ "from": "/osc.audio" }`, or `{ "from": "/osc" }` when the source has a single output —
//! an interface input pipe is such a source: `{ "from": "/in" }`). `config` carries
//! instantiate-time **`Constant`s** (e.g. a voicer's `voices`). Master output is the Signal
//! `interface.outputs` pipes (`"main_l": {"from": "/pan.left", "channel": 0}`); the v1
//! anonymous `outputs` block migrates into them at parse. Ports are referenced by **name**
//! (from the operator's [`Descriptor`](crate::descriptor::Descriptor)), not by brittle index.
//! Optional `doc` fields carry human/agent notes. Validity is defined by the loader itself
//!: [`load`] is the single authority on what a legal document is.
//!
//! [`load`] turns JSON into a [`Graph`] (resolving types via a [`Registry`]);
//! [`NormalizedDoc::from_graph`] goes the other way. Loading is an authoring step, not a realtime
//! path — it lives in the portable core but never runs on the audio thread.

mod normalize;

pub use normalize::NormalizedDoc;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::descriptor::{Curve, Descriptor, F32Meta, I32Meta, Port, PortType};
use crate::graph::{Graph, Interface, Node};
use crate::message::Arg;
use crate::operators::pipe::Pipe;
use crate::plan::PortKind;
use crate::registry::Registry;
use crate::resources::{ResolvedRefs, ResourceResolver, ResourceStore, SampleBuffer, SampleId};

/// The document format version this engine reads and writes. **v2**: the
/// `interface` direction flip — inputs/outputs are named pipes; the target-pointing entry form
/// and the top-level anonymous `outputs` block are v1-only, auto-migrated at parse. **v3**
///: presentation decouples from the instrument — the per-node `control` block and
/// `label`/`widget` on interface pipes retire; the loader drops leftovers with a
/// [`LoadWarning`] naming each (ignore-with-warning, never fatal).
pub const FORMAT_VERSION: u32 = 3;

fn default_format_version() -> u32 {
    1
}

/// The instrument name a bare [`scaffold_instrument`] mints when the caller names none.
pub const SCAFFOLD_DEFAULT_NAME: &str = "untitled";

/// Produce a **guaranteed-valid minimal instrument document** as JSON — the start move for
/// authoring an instrument from scratch (#146). First-creation stalls because the model, authoring
/// a top-level document from nothing, omits the required top-level `instrument` name field and
/// [`validate`](crate::introspect::validate) rejects the document (`InstrumentDoc` is
/// `deny_unknown_fields` with `instrument` required). Handing the author a valid seed turns
/// first-creation into the reshape-from-template path that already works: the model edits this
/// document and swaps it, instead of guessing the required shape.
///
/// The emitted document is the minimal one every door single-sources here (roster,
/// [`crate::tools`]): the current [`FORMAT_VERSION`], the required `instrument` name, and an empty
/// `nodes` list — `{ "format_version": 3, "instrument": <name>, "nodes": [] }`. Empty `nodes` is
/// valid (no registry needed to build it); every other field is optional and omitted. Built by
/// serializing an [`InstrumentDoc`] so the field spelling can only ever match the real serde
/// contract — the round-trip through `validate` is asserted `ok` in the introspect tests. Returns
/// the document **by value**; writing it to disk stays a native-only concern.
pub fn scaffold_instrument(name: Option<&str>) -> serde_json::Value {
    let doc = InstrumentDoc {
        format_version: FORMAT_VERSION,
        instrument: name.unwrap_or(SCAFFOLD_DEFAULT_NAME).to_string(),
        doc: None,
        resources: BTreeMap::new(),
        interface: None,
        nodes: Vec::new(),
        outputs: Vec::new(),
        migration: MigrationNotes::default(),
    };
    serde_json::to_value(&doc).expect("a minimal InstrumentDoc always serializes to JSON")
}

/// A complete instrument document.
///
/// This type and everything it reaches derive `JsonSchema` behind the default-off `schemars`
/// feature — not to publish a document schema (the format's authority is the loader, and an agent
/// is grounded, not schema-fed; see rules: agent-mcp), but so the **projection completeness guard**
/// can enumerate the format's leaf fields mechanically and fail the build when one lands with no
/// view in [`FIELD_COVERAGE`](crate::projection::FIELD_COVERAGE). The play/CLI build never compiles
/// schemars.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InstrumentDoc {
    /// Document format version. Absent means 1 — every document written before
    /// versioning is a valid v1 — and saving always writes the current version. Additive
    /// format changes (a new optional field) never bump it; a breaking shape change bumps it
    /// and ships a parse-time migration. A document **newer** than the engine understands is
    /// the fatal [`LoadError::UnsupportedVersion`]: the shape can't be trusted, so refusing
    /// with a clear message beats misloading.
    #[serde(default = "default_format_version")]
    pub format_version: u32,
    /// Human-facing name / id of this instrument.
    pub instrument: String,
    /// Optional note for humans and agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Decoded-resource table: logical id → source (a file path today). A node
    /// references a resource by id via its `sample` field; the loader resolves+decodes each
    /// referenced id once (dedup) into the [`ResourceStore`]. Entries no node uses are
    /// ignored.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resources: BTreeMap<String, String>,
    /// Engine-honored I/O boundary as named **pipes**:
    /// each `inputs` entry declares its type and mints an address internal nodes consume from;
    /// each `outputs` entry is fed from an internal port and — when Signal — is a master tap.
    /// A voice patch declares this so its host Voicer can drive its `freq`/`gate` pipes and tap
    /// its `audio`/`active`. Distinct from a node's `control`, which is opaque,
    /// engine-ignored UI metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<InterfaceDoc>,
    pub nodes: Vec<NodeDoc>,
    /// **v1-only**: the anonymous top-level master-tap list. Migration moves its
    /// entries into `interface.outputs` under generated names; a v2 document carrying one is a
    /// load error, and save never writes it (empty after migration).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<PortRef>,
    /// What the v1→v2 migration could not carry over — parse-session state, never
    /// serialized: [`build`](NormalizedDoc::build) surfaces the warnings and registers the dark names so
    /// references to a dropped entry degrade instead of failing. Empty on every
    /// native-v2 or cleanly migrated document.
    #[serde(skip)]
    pub(crate) migration: MigrationNotes,
}

/// See [`InstrumentDoc::migration`]: the non-fatal residue of a v1→v2 migration, carried from
/// parse (where migration runs, with no warning channel of its own) to build (which has one).
#[derive(Debug, Clone, Default)]
pub(crate) struct MigrationNotes {
    /// Warnings to surface on the next build of this document ([`LoadWarning::Migration`]).
    warnings: Vec<LoadWarning>,
    /// v1 `interface.inputs` names migration dropped (unexpressible in the pipe model): recorded
    /// as **dark** boundary inputs so a host wire onto the name drops with a warning — the same
    /// transitive degradation an unavailable nested target gets — instead of a fatal
    /// `UnknownInput` (a legal v1 document keeps loading).
    dark_inputs: BTreeSet<String>,
}

impl PartialEq for MigrationNotes {
    /// Notes are **parse-session state, not document identity**: two documents are equal iff
    /// their serialized forms are (the notes never serialize), so a freshly migrated document
    /// compares equal to its own save→reparse and to a hand-written native-v2 equivalent even
    /// when migration had something to say.
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

/// A document's `interface` block — the named I/O boundary as **pipes** (format
/// v2). Every `inputs` entry is an [`InputPipeDoc`]: it declares its `Arg` type and **mints an
/// address in the flat node namespace** (`in` → `/in`) that internal nodes consume with ordinary
/// wire-refs (`{"from": "/in"}`, fan-out free). Every `outputs` entry is an [`OutputPipeDoc`]
/// **fed from an internal port** (`"main_l": {"from": "/pan.left", "channel": 0}`). The v1
/// target-pointing forms still parse for migration and are rewritten at
/// the [`NormalizedDoc`] mint; they are illegal in a v2 document. Resolved at
/// [`build`](NormalizedDoc::build) into [`Interface`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InterfaceDoc {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InterfaceEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, InterfaceEntry>,
}

/// One `interface` entry. The v2 forms are [`Pipe`](Self::Pipe) (an input pipe, `{"type": …}`)
/// and [`Feed`](Self::Feed) (an output pipe, `{"from": …}`). The v1 target-pointing forms —
/// [`Target`](Self::Target) (a bare `"/node.port"` string) and [`Detailed`](Self::Detailed)
/// (`{"target": …}` plus presentational overrides) — parse so migration can rewrite
/// them; the loader rejects them un-migrated. Deserialization dispatches on the JSON shape and
/// the object's discriminator key (`type` / `from` / `target`) by hand, so a malformed object
/// keeps pointed field-level serde errors ("unknown field `lable`") instead of collapsing into
/// one opaque untagged no-variant error.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum InterfaceEntry {
    /// A v2 **input pipe**: declared type + optional channel binding + metadata.
    Pipe(InputPipeDoc),
    /// A v2 **output pipe**: fed from an internal port, optional channel binding.
    Feed(OutputPipeDoc),
    /// v1-only: the bare internal `/node.port` target (migrated at parse).
    Target(String),
    /// v1-only: target plus presentational-metadata overrides (migrated at parse).
    Detailed(InterfaceMeta),
}

/// A pipe's declared unwired/seed value: a number (`f32` / `f32_buffer` pipes) or a vocab-enum
/// variant **symbol** (enum pipes) — mirroring [`InputValue`]'s literal forms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum PipeDefault {
    Number(f64),
    Symbol(String),
}

/// A declared curve on an input pipe's numeric range (the good-button sweep hint), the document
/// spelling of [`Curve`]: `"lin"` or `"exp"` — the same tokens the operator contract macro uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub enum CurveDoc {
    #[serde(rename = "lin")]
    Lin,
    #[serde(rename = "exp")]
    Exp,
}

impl From<CurveDoc> for Curve {
    fn from(c: CurveDoc) -> Curve {
        match c {
            CurveDoc::Lin => Curve::Linear,
            CurveDoc::Exp => Curve::Exponential,
        }
    }
}

/// One v2 `interface.inputs` entry: a named **input pipe**. There is no inner
/// port to inherit from — the entry **declares its `Arg` type** (`"f32_buffer"`, `"f32"`,
/// `"i32"`, `"note"`, `"harmony"`, `"pitch"`, or a vocab enum name like `"FilterMode"`), and the declared type is
/// enforced against every consumer by the existing pass-2 wire check. A numeric pipe may carry
/// its own `default`/`min`/`max`/`curve` — **engine-enforced** on the pipe's port (literals and
/// external messages clamp to it; an unwired signal pipe materializes `default`, or silence when
/// bare). A **signal** pipe may bind a logical input `channel`, honored only when
/// the graph is played at top level; `channel` on a message pipe is a load error.
/// `label`/`unit`/`widget` are presentational (describe / control-surface generation).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InputPipeDoc {
    /// The declared `Arg` type: `"f32_buffer"` (Signal), `"f32"` (held Value), `"i32"` (held
    /// integer Value), `"note"` (Event), `"harmony"` (held Value), `"pitch"` (held Value), or a
    /// shared vocab enum's type name (`"FilterMode"`).
    #[serde(rename = "type")]
    pub ty: String,
    /// Logical input channel binding; signal pipes only, top-level-honored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<usize>,
    /// Unwired/seed value: number for a numeric pipe, variant symbol for an enum pipe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<PipeDefault>,
    /// Engine-enforced range floor (numeric pipes; defaults to the type-wide `NUMBER_MIN`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Engine-enforced range ceiling (numeric pipes; defaults to the type-wide `NUMBER_MAX`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    /// Sweep-curve hint for a numeric pipe (`"lin"`/`"exp"`); defaults to linear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve: Option<CurveDoc>,
    /// Display unit, e.g. `"Hz"` — describes the *quantity*, so it stays on the
    /// pipe and every surface inherits it (the engine enforces only the range).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// **Retired**: pipe display names live in a surface doc now. Deserialize-only
    /// sink; the [`NormalizedDoc`] mint drains it into a
    /// [`LoadWarning::DeprecatedPipePresentation`] and save never writes it.
    #[serde(default, skip_serializing)]
    pub(crate) label: Option<String>,
    /// **Retired**: widget hints live in a surface doc now. See `label`.
    #[serde(default, skip_serializing)]
    pub(crate) widget: Option<String>,
}

/// One v2 `interface.outputs` entry: a named **output pipe** fed from an internal
/// port. `from` is an ordinary wire-ref (`"/node.port"`, sole-output sugar allowed). A signal
/// pipe may bind the logical master `channel` it feeds (omitted = today's broadcast meaning);
/// `channel` on a message-typed pipe is a load error. `label`/`unit`/`widget` are
/// presentational; `min`/`max` presentational overrides obey the subset law against
/// the feeding port's engine-enforced range.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct OutputPipeDoc {
    /// The internal `/node.port` (or sole-output `/node`) wire-ref feeding this pipe.
    pub from: String,
    /// Logical master output channel this pipe feeds; omitted = broadcast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<usize>,
    /// **Retired**: pipe display names live in a surface doc now. Deserialize-only
    /// sink; the [`NormalizedDoc`] mint drains it into a
    /// [`LoadWarning::DeprecatedPipePresentation`] and save never writes it.
    #[serde(default, skip_serializing)]
    pub(crate) label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// **Retired**: widget hints live in a surface doc now. See `label`.
    #[serde(default, skip_serializing)]
    pub(crate) widget: Option<String>,
    /// Presentational range-minimum override (subset of the feeding port's range).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Presentational range-maximum override (see `min`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// The v1 object entry form: the internal target plus
/// presentational overrides. Parses so migration can rewrite it into the pipe forms; never
/// written back (save writes v2). `deny_unknown_fields` keeps its field-level errors pointed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct InterfaceMeta {
    /// The internal `/node.port` wire-ref this external name resolves to.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

impl InterfaceEntry {
    /// The v2 input-pipe form, if this entry is one.
    pub fn pipe(&self) -> Option<&InputPipeDoc> {
        match self {
            InterfaceEntry::Pipe(p) => Some(p),
            _ => None,
        }
    }

    /// The v2 output-pipe form, if this entry is one.
    pub fn feed(&self) -> Option<&OutputPipeDoc> {
        match self {
            InterfaceEntry::Feed(f) => Some(f),
            _ => None,
        }
    }

    /// The v1 target this entry points at, if it is an un-migrated v1 form.
    fn v1_target(&self) -> Option<&str> {
        match self {
            InterfaceEntry::Target(t) => Some(t),
            InterfaceEntry::Detailed(m) => Some(&m.target),
            _ => None,
        }
    }

    /// The v1 presentational overrides, when the entry carries any (the v1 object form).
    fn v1_meta(&self) -> Option<&InterfaceMeta> {
        match self {
            InterfaceEntry::Detailed(m) => Some(m),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for InterfaceEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        // Buffer the value so the object form can dispatch on its discriminator key. Cold path
        // (parse-time only); `from_value` keeps the concrete struct's pointed field errors.
        let v = serde_json::Value::deserialize(deserializer)?;
        match v {
            serde_json::Value::String(s) => Ok(InterfaceEntry::Target(s)),
            serde_json::Value::Object(ref map) => {
                type Parse = fn(serde_json::Value) -> Result<InterfaceEntry, serde_json::Error>;
                let picks: [(&str, Parse); 3] = [
                    ("type", |v| {
                        serde_json::from_value(v).map(InterfaceEntry::Pipe)
                    }),
                    ("from", |v| {
                        serde_json::from_value(v).map(InterfaceEntry::Feed)
                    }),
                    ("target", |v| {
                        serde_json::from_value(v).map(InterfaceEntry::Detailed)
                    }),
                ];
                let hits: Vec<_> = picks.iter().filter(|(k, _)| map.contains_key(*k)).collect();
                match hits.as_slice() {
                    [(_, parse)] => parse(v).map_err(D::Error::custom),
                    [] => Err(D::Error::custom(
                        "an interface entry object needs `type` (an input pipe) or `from` (an \
                         output pipe); the v1 `target` form is also accepted for migration",
                    )),
                    _ => Err(D::Error::custom(
                        "an interface entry object may carry only one of `type` (input pipe), \
                         `from` (output pipe), or `target` (v1)",
                    )),
                }
            }
            _ => Err(D::Error::custom(
                "an interface entry is an object (`{\"type\": …}` input pipe / `{\"from\": …}` \
                 output pipe) or a v1 \"/node.port\" target string",
            )),
        }
    }
}

/// One operator instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct NodeDoc {
    /// Operator type name (must be registered, e.g. `"oscillator"`).
    #[serde(rename = "type")]
    pub type_name: String,
    /// OSC address / routing prefix, e.g. `"/osc"`. Unique within the instrument.
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Instantiate-time **`Constant`s** by name, e.g. `{ "voices": 8 }`. A name here
    /// must be a declared [`Constant`](Descriptor::constants); a runtime input set here, or a
    /// constant set in `inputs`, is a load error.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, ConfigValue>,
    /// One value per wired/settable input: a **literal** (a number, or an `Enum` symbol
    /// like `"Hp"`) or a **wire-ref** (`{ "from": "/node.port" }`). Replaces the old `params` map
    /// and top-level `connections` array — a `Float` input and the wire that drives it now target
    /// the same slot. Omitted inputs use the descriptor default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InputValue>,
    /// Resource reference: a logical id into the document's `resources` table.
    /// Only valid on an operator whose descriptor declares a `sample` resource slot (the
    /// sample player); rejected elsewhere as a structural [`LoadError::UnknownResource`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
    /// Instrument-resource reference: a logical id into the document's `resources`
    /// table whose source is a **voice patch** (a standalone instrument JSON). Only valid on an
    /// operator declaring a `voice` resource slot (the Voicer); rejected elsewhere as a structural
    /// [`LoadError::UnknownResource`]. The loader builds the patch `voices` times and binds the
    /// graphs via [`Operator::bind_voices`](crate::operator::Operator::bind_voices).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Nested-instrument reference: a logical id into the document's `resources` table
    /// whose source is another instrument patch. Only valid on the built-in `subpatch` operator
    /// (which declares a `patch` resource slot); rejected elsewhere as a structural
    /// [`LoadError::UnknownResource`]. At build the referenced patch is loaded recursively and
    /// **inlined** (nesting P4): its nodes are spliced into this graph under this
    /// node's address prefix, the boundary face is synthesized from its `interface`, and the
    /// `subpatch` node dissolves — it never reaches the built [`Graph`]. The reference survives in
    /// the *document* only — the document is the save source of truth, and `from_graph` of a
    /// built graph is the explicit flatten/export path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    /// **Retired**: the v2 per-node `control` block.
    /// Deserialize-only sink so a v2 document (or a v3 one still carrying leftovers) parses
    /// under `deny_unknown_fields`; the [`NormalizedDoc`] mint drains it into a
    /// [`LoadWarning::DeprecatedControlBlock`] and save never writes it — presentation lives
    /// in a surface doc now.
    #[serde(default, skip_serializing)]
    pub(crate) control: Option<serde_json::Value>,
}

impl NodeDoc {
    /// This node's resource references paired with the slot each targets: the
    /// typed `sample`/`voice`/`patch` fields surfaced as one `(slot, ref)` list. The single place the
    /// format maps its fields to descriptor [`ResourceSlot`](crate::descriptor::ResourceSlot)
    /// names, so generic resource validation iterates this rather than enumerating known slots arm
    /// by arm — a new slot extends this list and nothing downstream.
    fn resource_refs(&self) -> [(&'static str, &Option<String>); 3] {
        [
            ("sample", &self.sample),
            ("voice", &self.voice),
            ("patch", &self.patch),
        ]
    }
}

/// One [`NodeDoc::inputs`] value: a wire-ref, an `Enum` symbol, or a numeric literal.
///
/// Untagged: a JSON object `{ "from": ... }` is a [`Wire`](Self::Wire); a JSON string is a
/// [`Symbol`](Self::Symbol) (an `Enum` variant name); a JSON number is a [`Number`](Self::Number)
/// (a `Float`/param value, or an `Enum` index fallback).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum InputValue {
    /// A wire-ref to a source output: `"/node.port"`, or `"/node"` when the source has exactly
    /// one output (the sole-output sugar).
    Wire { from: String },
    /// An `Enum` input symbol, e.g. `"Hp"` (enum-over-the-wire, symbol primary).
    Symbol(String),
    /// A numeric literal — a `Float` input/param value, or an `Enum` variant index (fallback form).
    Number(f64),
}

/// One [`NodeDoc::config`] value: an instantiate-time `Constant`.
///
/// Untagged: a JSON number is a [`Number`](Self::Number) (an `Int` constant such as `voices`); a
/// JSON string is a [`Symbol`](Self::Symbol) (an `Enum` constant, none today). Floats are accepted
/// and rounded so `8` and `8.0` both name 8 voices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ConfigValue {
    /// An `Int` constant (e.g. `voices`), applied rounded.
    Number(f64),
    /// An `Enum` constant symbol (none today; forward-compatible).
    Symbol(String),
}

/// A reference to one node's port, by names. Used only in `outputs` (a master tap);
/// node-to-node wiring lives in [`NodeDoc::inputs`] as a [`InputValue::Wire`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct PortRef {
    pub node: String,
    pub port: String,
    /// Logical master channel this tap feeds: `0` = first channel (left), `1` = second
    /// (right), and so on. Omitted → broadcast to every channel (the historical mono fan; existing
    /// instruments are bit-identical).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<usize>,
}

/// Why loading an instrument document failed. Messages are written for an author
/// (human or agent) to act on.
#[derive(Debug)]
pub enum LoadError {
    /// The JSON itself was malformed.
    Json(serde_json::Error),
    /// The document declares a `format_version` newer than this engine understands
    ///: its shape can't be trusted, so refusing beats misloading. Older versions
    /// migrate at parse; only the future is unreadable.
    UnsupportedVersion { found: u32, supported: u32 },
    /// A node names an operator type that isn't registered.
    UnknownType { address: String, type_name: String },
    /// Two nodes share an address.
    DuplicateAddress(String),
    /// A wire-ref or output references a node that doesn't exist.
    UnknownNode(String),
    /// A node has no port with that name (in the required direction).
    UnknownPort { node: String, port: String },
    /// A node has no input (port, settable param, or enum) with that name.
    UnknownInput { node: String, input: String },
    /// An `inputs` entry sets a value the descriptor can't read as that input — an `Enum` symbol on
    /// a non-enum input, or a symbol/index that names no variant (never snaps to default).
    BadInputValue {
        node: String,
        input: String,
        value: String,
    },
    /// A `config` name is not a declared [`Constant`](Descriptor::constants).
    UnknownConfig { node: String, name: String },
    /// A `Constant` (e.g. `voices`) appears in `inputs` — it must live in `config`, since changing
    /// it would rebuild the graph.
    ConstantInInputs { node: String, name: String },
    /// A wire-ref uses the sole-output sugar (`"/node"`) but the source has more than one output,
    /// so the intended port is ambiguous.
    AmbiguousWire { node: String, reference: String },
    /// A wire joins two ports of incompatible [`PortType`]s (e.g. `Note` → `Buffer`) — the illegal
    /// wiring. Equal types are fine, an `F32` source into a `Buffer` port is the one
    /// implicit ZOH bridge (the reverse, `Buffer` → `F32`, is Signal→Value and rejected:
    /// no implicit sample-and-hold), and an [`Arg`](PortType::Arg) pass-through input takes any
    /// source with an OSC form; everything else is rejected here. On a nested boundary wire,
    /// `from`/`to` name the **boundary** port (`/sub.audio`), never the prefixed internals
    ///.
    TypeMismatch {
        from: String,
        from_type: Box<PortType>,
        to: String,
        to_type: Box<PortType>,
    },
    /// A node carries a `sample` reference but its operator declares no such resource slot
    /// — a structural misuse, fatal like the other wiring errors.
    UnknownResource { node: String, slot: String },
    /// An instrument-resource `source` is referenced again while it is still being loaded — a
    /// `voice`/`patch` chain that (directly or transitively) contains itself.
    /// Loading it would recurse forever, so the cycle is a structural error, fatal like the
    /// other wiring errors.
    CyclicResource { source: String },
    /// An `interface.outputs` entry's presentational range override lies about what the engine
    /// enforces: a range override on a port with no numeric range, a bound outside
    /// the feeding port's engine-clamped range, or an inverted/empty advertised range.
    /// `describe` publishes these values as the boundary contract and no engine path reconciles
    /// them, so advertised must stay a subset of enforced — checked here at load, named by the
    /// boundary port. (Input pipes own their range outright, so this law now
    /// applies to outputs, and to v1 input entries during migration.)
    InterfaceOverride { name: String, reason: String },
    /// An `interface` **pipe** entry is malformed: an unknown declared type, a
    /// `channel` on a message pipe, numeric metadata on a non-numeric pipe, an incoherent
    /// declared range/default, a v1 target-form entry surviving in a v2 document, or a v1 entry
    /// migration cannot express. Named by the boundary entry.
    InterfacePipe { name: String, reason: String },
    /// A v2 document carries the top-level anonymous `outputs` block, which is v1-only
    ///: master taps are declared as `interface.outputs` pipes now.
    AnonymousOutputs,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Json(e) => write!(f, "invalid JSON: {e}"),
            LoadError::UnsupportedVersion { found, supported } => write!(
                f,
                "document is format version {found}, but this engine reads at most \
                 {supported} — upgrade reuben to load it"
            ),
            LoadError::UnknownType { address, type_name } => {
                write!(f, "node {address}: unknown operator type {type_name:?}")
            }
            LoadError::DuplicateAddress(a) => write!(f, "duplicate node address {a:?}"),
            LoadError::UnknownNode(n) => write!(f, "reference to unknown node {n:?}"),
            LoadError::UnknownPort { node, port } => {
                write!(f, "node {node:?} has no port {port:?}")
            }
            LoadError::UnknownInput { node, input } => {
                write!(f, "node {node:?} has no input {input:?}")
            }
            LoadError::BadInputValue { node, input, value } => {
                write!(f, "node {node:?} input {input:?}: invalid value {value:?}")
            }
            LoadError::UnknownConfig { node, name } => {
                write!(f, "node {node:?} has no config constant {name:?}")
            }
            LoadError::ConstantInInputs { node, name } => write!(
                f,
                "node {node:?}: {name:?} is a constant — set it in `config`, not `inputs`"
            ),
            LoadError::AmbiguousWire { node, reference } => write!(
                f,
                "node {node:?}: wire-ref {reference:?} is ambiguous (source has multiple outputs; \
                 name one as \"/node.port\")"
            ),
            LoadError::TypeMismatch {
                from,
                from_type,
                to,
                to_type,
            } => {
                write!(
                    f,
                    "wire {from} ({from_type}) -> {to} ({to_type}): incompatible port types"
                )?;
                // The one near-miss worth a hint: audio into a scalar control looks plausible
                // (the legal ZOH bridge runs the other way) — point at the sanctioned path.
                if from_type.is_buffer() && matches!(**to_type, PortType::F32) {
                    write!(
                        f,
                        " — no implicit sample-and-hold; wire an explicit sig→val converter \
                         (envelope follower / quantizer)"
                    )?;
                }
                Ok(())
            }
            LoadError::UnknownResource { node, slot } => {
                write!(f, "node {node:?} has no resource slot {slot:?}")
            }
            LoadError::CyclicResource { source } => write!(
                f,
                "instrument resource {source:?} references itself (directly or transitively) — \
                 cyclic nesting cannot load"
            ),
            LoadError::InterfaceOverride { name, reason } => {
                write!(f, "interface entry {name:?}: {reason}")
            }
            LoadError::InterfacePipe { name, reason } => {
                write!(f, "interface pipe {name:?}: {reason}")
            }
            LoadError::AnonymousOutputs => write!(
                f,
                "the top-level anonymous `outputs` block is format v1 — declare named \
                 `interface.outputs` pipes instead"
            ),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Json(e) => Some(e),
            _ => None,
        }
    }
}

/// Parse JSON and build the [`Graph`], resolving operator types via `registry`.
///
/// This path resolves no resources — a sample-bearing instrument loaded this way binds its
/// players to nothing (they play silence), and a nested `subpatch` reference dissolves dark
/// (nothing spliced in; wires touching it dropped — see [`NormalizedDoc::build`]). Use
/// [`load_instrument`] to resolve, bind, and inline.
pub fn load(json: &str, registry: &Registry) -> Result<Graph, LoadError> {
    NormalizedDoc::from_json(json, registry, None)?.build(registry)
}

/// A non-fatal resource problem found at load. The owning node still builds and
/// binds to an empty buffer (so it plays silence); the boundary surfaces these to the user
/// because they are authoring errors, just not crashing ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadWarning {
    /// A node names a resource id absent from the `resources` table. `slot` is the resource slot
    /// the ref targeted (`"sample"`/`"voice"`/`"patch"`, [`NodeDoc::resource_refs`]) so the
    /// message names what actually failed.
    MissingResource {
        node: String,
        slot: &'static str,
        id: String,
    },
    /// A resource id resolves to a source that could not be loaded/decoded. `slot` as on
    /// [`MissingResource`](Self::MissingResource).
    ResolveFailed {
        slot: &'static str,
        id: String,
        source: String,
        reason: String,
    },
    /// A warning that arose while loading a **nested** instrument (a voice or subpatch child),
    /// contextualized by the referencing parent node so provenance survives the
    /// merge into the parent's warning list: warnings surface during the child's own load, while
    /// its addresses are still child-relative (inline prefixing happens after), and two
    /// same-shaped children would otherwise be indistinguishable. Nests recursively for deeper
    /// chains.
    Nested {
        node: String,
        warning: Box<LoadWarning>,
    },
    /// An `interface` entry was dropped because its internal target is dark — an unavailable
    /// nested child, or (recursively) a boundary port that itself went dark a level down. The
    /// port is real in the document but resolves to nothing this load; it is recorded on
    /// [`Interface::dark_inputs`](crate::graph::Interface)/`dark_outputs` so a consumer
    /// referencing it degrades the same way (wire dropped, this warning) instead of hitting a
    /// fatal `UnknownInput`/`UnknownPort` one level up — dark degradation stays **transitive**
    ///.
    DarkInterfaceEntry {
        /// The external interface name that vanished.
        name: String,
        /// The internal reference it pointed at (`"/inner.freq"`).
        target: String,
    },
    /// A `subpatch` node carries no `patch` reference at all — an authoring
    /// mistake, not an availability failure, but the node still dissolves dark so the
    /// instrument stays playable. Warned rather than silent: pre-inline this shape
    /// failed loud, and silence through the nest would hide the typo.
    NoPatchRef { node: String },
    /// A nested child's **bare signal input pipe** (no declared default) is left unfed by the
    /// hosting node — neither a wire nor a literal lands on the boundary name — so the pipe
    /// renders **silence**. Warned, never fatal: silence is honest, but a nest
    /// whose audio input nobody wired is usually an authoring slip. A pipe with a declared
    /// default (a control) materializes that default unfed — normal knob behavior, no warning —
    /// and message pipes are silent-by-nature streams.
    UnwiredPipe { node: String, name: String },
    /// The v1→v2 migration could not carry part of the v1 document
    /// into the pipe model faithfully — an entry whose target port type has no pipe form, an
    /// entry aliasing a target another entry already flipped, an entry whose target the child
    /// wires internally (v1's merge-legal state the flip cannot express), or a node renamed out
    /// of a minted pipe address's way. Never silent (migration loses nothing quietly) and never
    /// fatal ("v1 documents keep loading forever"): the document loads, degraded exactly as
    /// `detail` says, and re-authoring the entry in v2 form retires the warning. `name` is the
    /// interface entry (or node address) affected.
    Migration { name: String, detail: String },
    /// A **top-level** bare signal input pipe (no declared default) binds no logical input
    /// channel. At top level nothing else can ever feed a signal pipe — there is
    /// no parent edge, and audio does not cross the OSC boundary — so it renders **silence**.
    /// Warned, never fatal (dark-degrade): the patch still plays, but a declared
    /// audio input nobody bound is usually an authoring slip. A pipe with a declared default
    /// is a control at rest (no warning); nested/hosted pipes get [`UnwiredPipe`](Self::UnwiredPipe)
    /// from their host instead.
    UnboundInputPipe { name: String },
    /// A **hosted voice** patch's input pipe binds a logical input channel, which is **inert**
    /// when hosted: a pipe inside a nest is never a magic hardware connection —
    /// the host's edges feed a voice's pipes, and an unfed one renders silence. The binding
    /// still works when the same patch is played at top level, so this is a warning, not an
    /// error. Wrapped in [`Nested`](Self::Nested) with the hosting node.
    InertChannelBinding { name: String },
    /// The node carried a retired v2 `control` block — UI
    /// metadata the engine never read. Ignored, never fatal: sound is unaffected; presentation
    /// lives in a surface doc now. Re-saving the document strips it.
    DeprecatedControlBlock { node: String },
    /// The interface entry carried retired pipe presentation — `label`/`widget`. Ignored,
    /// never fatal: the pipe keeps its quantity contract
    /// (`type`/`default`/`min`/`max`/`curve`/`unit`); presentation lives in a surface doc now.
    /// Re-saving the document strips it. `field` names which retired field was dropped.
    DeprecatedPipePresentation { name: String, field: &'static str },
}

impl fmt::Display for LoadWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadWarning::MissingResource { node, slot, id } => {
                write!(f, "node {node:?}: {slot} {id:?} not in resources table")
            }
            LoadWarning::ResolveFailed {
                slot,
                id,
                source,
                reason,
            } => {
                write!(f, "{slot} {id:?} ({source:?}): {reason}")
            }
            LoadWarning::Nested { node, warning } => write!(f, "in {node:?}: {warning}"),
            LoadWarning::DarkInterfaceEntry { name, target } => write!(
                f,
                "interface entry {name:?} dropped: its target {target:?} is dark (unavailable nested patch)"
            ),
            LoadWarning::NoPatchRef { node } => write!(
                f,
                "node {node:?}: subpatch has no `patch` reference — nothing inlined (plays silence)"
            ),
            LoadWarning::UnwiredPipe { node, name } => write!(
                f,
                "node {node:?}: input pipe {name:?} is unfed — it renders silence"
            ),
            LoadWarning::Migration { name, detail } => {
                write!(f, "v1 migration, {name:?}: {detail}")
            }
            LoadWarning::UnboundInputPipe { name } => write!(
                f,
                "top-level input pipe {name:?} binds no logical input channel — nothing can \
                 feed it; it renders silence"
            ),
            LoadWarning::InertChannelBinding { name } => write!(
                f,
                "input pipe {name:?} binds a logical input channel, which is inert in a hosted \
                 voice — the host's edges feed a voice's pipes; unfed, it renders silence"
            ),
            LoadWarning::DeprecatedControlBlock { node } => write!(
                f,
                "node {node:?}: dropped retired `control` block — presentation \
                 lives in a surface doc; re-saving writes it away"
            ),
            LoadWarning::DeprecatedPipePresentation { name, field } => write!(
                f,
                "interface entry {name:?}: dropped retired `{field}` — pipe \
                 presentation lives in a surface doc; re-saving writes it away"
            ),
        }
    }
}

impl LoadWarning {
    /// Wrap `self` with the parent node that referenced the nested instrument it arose in.
    fn nested_in(self, node: &str) -> LoadWarning {
        LoadWarning::Nested {
            node: node.to_string(),
            warning: Box::new(self),
        }
    }
}

/// The result of [`load_instrument`]: the built graph (resources bound) plus any non-fatal
/// [`LoadWarning`]s. Core returns structured warnings; the boundary decides how to present
/// them.
pub struct Loaded {
    pub graph: Graph,
    pub warnings: Vec<LoadWarning>,
}

/// Shared state threaded through the recursive nested-load passes (`voice`/`patch`),
/// one per top-level load.
#[derive(Default)]
struct LoadCtx {
    /// The cycle-guard stack: instrument-resource sources currently being resolved,
    /// root-first, keyed by **canonical id** ([`ResourceResolver::canonical`]) so two spellings
    /// of one source (`a.json` vs `./a.json`) are one identity. A chain that
    /// re-enters a source still on the stack is the fatal [`LoadError::CyclicResource`]
    /// instead of infinite recursion.
    loading: Vec<String>,
    /// Decoded-sample cache, keyed by canonical id (the same identity the cycle guard
    /// uses). Each subpatch reuse and voice copy builds its own graph and
    /// [`ResourceStore`], but a given source is fetched + decoded **once** per load; the
    /// stores share the `Arc`. Failures are deliberately not cached, so every referencing
    /// document still surfaces its own warning.
    samples: BTreeMap<String, Arc<SampleBuffer>>,
    /// Parsed (normalized) child-document cache, keyed by canonical id: a child source is read
    /// and parsed **once** per load, however many re-exported v1 entries
    /// (the migration's child re-export arm) or `subpatch` nodes reference it.
    /// Typed [`NormalizedDoc`] so recursive child parses carry the mint's invariant too.
    /// Successes only — a resolve/parse failure is re-surfaced per referencing site so each
    /// keeps its own warning (the `samples` policy).
    docs: BTreeMap<String, NormalizedDoc>,
}

/// Parse, build, and **resolve + bind decoded resources** — the full authoring
/// load path. Structural/wiring problems are fatal ([`LoadError`]); resource problems are
/// non-fatal: a missing id or a resolve/decode failure binds the node to an empty buffer
/// (it plays silence) and is reported as a [`LoadWarning`]. Each referenced id is resolved
/// exactly once (dedup) via `resolver`; unreferenced `resources` entries are ignored.
pub fn load_instrument(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<Loaded, LoadError> {
    load_instrument_guarded(json, registry, resolver, &mut LoadCtx::default(), None)
}

/// [`load_instrument`] from an already-minted document — the parse-once entry point for a
/// caller that needs both the document (e.g. for its `interface` overrides,
/// [`crate::describe::describe_boundary`]) and the built graph, without re-parsing the JSON.
/// Takes [`NormalizedDoc`] by type: the version gate and migration ran exactly once at the
/// mint, so this path has nothing to re-check — a host holding a raw
/// [`InstrumentDoc`] enters via [`NormalizedDoc::from_doc`].
pub fn load_instrument_doc(
    doc: &NormalizedDoc,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<Loaded, LoadError> {
    load_doc_guarded(doc, registry, resolver, &mut LoadCtx::default(), None)
}

/// [`load_instrument`] with the shared load state threaded through: `ctx` carries the cycle
/// guard (a chain that re-enters a source still loading is caught as
/// [`LoadError::CyclicResource`] instead of recursing forever) and the per-load decoded-sample
/// cache the recursive passes (`voice`, `patch`) share. `referrer` is the canonical id of the
/// document being loaded (`None` at the top level), threaded to
/// [`ResourceResolver::canonical`] so a nested document's own references resolve relative to
/// *its* location.
fn load_instrument_guarded(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    ctx: &mut LoadCtx,
    referrer: Option<&str>,
) -> Result<Loaded, LoadError> {
    let doc = NormalizedDoc::parse_with(json, registry, Some(resolver), ctx, referrer)?;
    load_doc_guarded(&doc, registry, resolver, ctx, referrer)
}

/// [`load_instrument_guarded`] from an already-minted document — the subpatch pass parses a
/// shared child source once and loads it per referencing node through this. The old defensive
/// re-normalize that lived here is gone: [`NormalizedDoc`] *is* the proof the gate + migration
/// already ran, so a gate-bypassing document cannot reach this path by construction.
fn load_doc_guarded(
    doc: &NormalizedDoc,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    ctx: &mut LoadCtx,
    referrer: Option<&str>,
) -> Result<Loaded, LoadError> {
    // Build with the resolver threaded in (the resolution-ordering note): a `subpatch`
    // node's boundary face only exists once its child is resolved, and it must exist *during*
    // build pass 2 so the one wire type-checker covers boundary wires — so nested references
    // resolve and inline inside `build`, earlier than the pipeline resolves `sample`/
    // `voice` refs below.
    let Loaded {
        mut graph,
        mut warnings,
    } = doc.build_nested(registry, Some(resolver), ctx, referrer)?;

    // Resolve every referenced id once into the store; record id -> handle for binding. The
    // fetch + decode goes through the load-wide source cache (`LoadCtx::samples`), so N
    // subpatch reuses or voice copies of a sample-heavy child decode each source once and
    // share the buffer.
    let mut store = ResourceStore::new();
    let mut handles: BTreeMap<String, SampleId> = BTreeMap::new();
    for n in &doc.nodes {
        let Some(id) = &n.sample else { continue };
        if handles.contains_key(id) {
            continue; // dedup: already resolved by an earlier node
        }
        let buffer = match lookup_source(doc, &n.address, "sample", id, &mut warnings) {
            None => Arc::new(SampleBuffer::empty()),
            Some(source) => {
                // Canonical id is the cache key, so one sample spelled two ways still
                // fetches + decodes once (the identity rule, applied to samples).
                let canon = resolver.canonical(source, referrer);
                match ctx.samples.get(&canon) {
                    Some(shared) => shared.clone(),
                    None => match resolver.resolve(&canon) {
                        Ok(b) => {
                            let shared = Arc::new(b);
                            ctx.samples.insert(canon, shared.clone());
                            shared
                        }
                        Err(e) => {
                            // Not cached: every referencing document keeps its own warning.
                            warnings.push(LoadWarning::ResolveFailed {
                                slot: "sample",
                                id: id.clone(),
                                source: canon,
                                reason: e.to_string(),
                            });
                            Arc::new(SampleBuffer::empty())
                        }
                    },
                }
            }
        };
        handles.insert(id.clone(), store.insert(id.clone(), buffer));
    }

    let store = Arc::new(store);

    // Bind each resource-bearing node's op (spawn carries the binding to any voice copies).
    for n in &doc.nodes {
        let Some(id) = &n.sample else { continue };
        let handle = handles[id];
        let mut refs = ResolvedRefs::new();
        refs.set("sample", handle);
        if let Some(key) = graph.find(&n.address) {
            graph.nodes[key].op.bind_resources(&store, &refs);
        }
    }

    // Instrument-resource pass: a node with a `voice` ref hosts N copies of a voice
    // patch. Build the patch `voices` times (each an independent Graph — `Graph` is not `Clone`, and
    // `Plan::instantiate` consumes one Graph per voice) and bind them; the Voicer turns them into
    // per-voice sub-plans at `on_instantiate`. Structural errors in the patch are fatal; a missing id
    // or resolve failure degrades to silence (empty graphs) with a warning, like a sample.
    for n in &doc.nodes {
        let Some(id) = &n.voice else { continue };
        let Some(key) = graph.find(&n.address) else {
            continue;
        };
        let n_voices = voice_count(&graph.nodes[key]);
        let mut voices: Vec<Graph> = Vec::with_capacity(n_voices);
        match lookup_source(doc, &n.address, "voice", id, &mut warnings) {
            None => {
                for _ in 0..n_voices {
                    voices.push(Graph::new());
                }
            }
            Some(source) => {
                let canon = resolver.canonical(source, referrer);
                for i in 0..n_voices {
                    let loaded =
                        resolve_instrument_slotted(&canon, "voice", registry, resolver, ctx)?;
                    // One copy's warnings suffice — the N builds are identical. Wrapped with the
                    // hosting node so provenance survives the merge (see `LoadWarning::Nested`).
                    if i == 0 {
                        warnings
                            .extend(loaded.warnings.into_iter().map(|w| w.nested_in(&n.address)));
                        // A hosted voice's **bare signal** input pipe is never fed: the Voicer
                        // drives the message pipes it knows by name (`freq`/`gate`) and nothing
                        // else reaches a hosted boundary, so the pipe renders silence — warn,
                        // exactly as the subpatch face does ("nested/hosted").
                        // A channel-bound bare pipe warns here *and* `InertChannelBinding`
                        // below, matching the subpatch path: the two warnings say different
                        // things (this pipe is silent; that binding is inert).
                        for (name, (key, _)) in &loaded.graph.interface.inputs {
                            let p = &loaded.graph.nodes[*key].descriptor.inputs[0];
                            if p.ty.is_buffer() && p.meta.is_none() {
                                warnings.push(LoadWarning::UnwiredPipe {
                                    node: n.address.clone(),
                                    name: name.clone(),
                                });
                            }
                        }
                        // A hosted voice's channel bindings are inert: the
                        // Voicer clears them before instantiating the sub-plan, so a bound
                        // pipe the host doesn't drive renders silence. Say so at load, once.
                        for pipe in loaded.graph.interface.input_channels.keys() {
                            warnings.push(
                                LoadWarning::InertChannelBinding { name: pipe.clone() }
                                    .nested_in(&n.address),
                            );
                        }
                    }
                    // Hosted inertness is enforced *here*, once, for every copy: clearing a hosted voice graph's channel bindings before
                    // any host
                    // sees it means `Plan::instantiate` derives no input-master plumbing for
                    // voice sub-plans — no host operator has to remember caller-side
                    // mutation, and the pipes stay on the message-fed materialize path the
                    // Voicer drives (`freq`/`gate`).
                    let mut voice = loaded.graph;
                    voice.interface.input_channels.clear();
                    voices.push(voice);
                }
            }
        }
        graph.nodes[key].op.bind_voices(voices);
    }

    Ok(Loaded { graph, warnings })
}

/// Look up a node's resource `id` in the document's `resources` table — the shared first step of
/// every resource pass (`sample`/`voice`/`patch`). A miss is the non-fatal
/// [`LoadWarning::MissingResource`], pushed here so the policy lives in one place;
/// each caller picks its degradation on `None` (empty buffer, silent voices, no sub-graph).
fn lookup_source<'a>(
    doc: &'a InstrumentDoc,
    node: &str,
    slot: &'static str,
    id: &str,
    warnings: &mut Vec<LoadWarning>,
) -> Option<&'a String> {
    let source = doc.resources.get(id);
    if source.is_none() {
        warnings.push(LoadWarning::MissingResource {
            node: node.to_string(),
            slot,
            id: id.to_string(),
        });
    }
    source
}

/// The voice-pool size for a Voicer node: the node's **stored** value for the
/// operator's instantiate-time [`Constant`](Descriptor::constants) (the voicer's `voices` pool
/// size), else that Constant's descriptor default, floored to 1. Reads the coerce-clamped
/// `constant_overrides` entry the config pass stored through the single coercion seam
/// ([`Graph::set_constant`] → [`Port::coerce`]) rather than re-reading the raw config number, so
/// the pool this pass builds always agrees with the constant a save/reload round-trips
/// — an out-of-range `voices` can't build one pool size and store another. Reads the
/// generic first Constant slot rather than a hardcoded `"voices"` name, so the same machinery
/// serves any future pool-sized operator. An operator with no Constant has a pool of one.
fn voice_count(node: &Node) -> usize {
    let Some(constant) = node.descriptor.constants.first() else {
        return 1;
    };
    let default = match &constant.ty {
        PortType::I32 { meta: Some(m) } => i64::from(m.default),
        _ => 1,
    };
    let stored = node
        .constant_overrides
        .iter()
        .find(|(slot, _)| *slot == 0)
        .map(|(_, arg)| match arg {
            Arg::I32(v) => i64::from(*v),
            Arg::F32(v) => v.round() as i64,
            _ => default,
        })
        .unwrap_or(default);
    stored.max(1) as usize
}

/// Resolve an **instrument-kind resource**: a patch `source` (a path) is read to its
/// JSON via [`ResourceResolver::resolve_text`], then built into a sub-[`Graph`] through the full
/// [`load_instrument`] path — so the sub-patch's own `sample` resources resolve recursively and its
/// `interface` boundary is resolved for the host to bind. Structural/wiring problems in the patch
/// are fatal ([`LoadError`]) — including JSON that fails to parse: the split lands at the
/// fetch seam, so only *availability* degrades. A `resolve_text` failure is
/// **non-fatal**: it yields an empty graph plus a [`LoadWarning::ResolveFailed`], so one missing
/// voice patch never crashes the host. A `source` that (directly or transitively) references itself is fatal
/// ([`LoadError::CyclicResource`]) — the cycle guard that keeps recursive nesting
/// from overflowing the stack. This is the net-new piece needed — "a resource that is a
/// Graph, not bytes."
pub fn resolve_instrument(
    source: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<Loaded, LoadError> {
    let canon = resolver.canonical(source, None);
    resolve_instrument_slotted(&canon, "patch", registry, resolver, &mut LoadCtx::default())
}

/// [`resolve_instrument`] with the cycle guard and the resource `slot` the ref came through (so
/// warnings name what actually failed), folding an unavailable source into the
/// degradation the voice pass wants: an empty graph carrying the warning (silence).
/// `source` is already canonical — every caller canonicalizes at the site that knows the
/// referring document.
fn resolve_instrument_slotted(
    source: &str,
    slot: &'static str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    ctx: &mut LoadCtx,
) -> Result<Loaded, LoadError> {
    match try_resolve_instrument(source, slot, registry, resolver, ctx)? {
        Ok(loaded) => Ok(loaded),
        Err(warning) => Ok(Loaded {
            graph: Graph::new(),
            warnings: vec![warning],
        }),
    }
}

/// The distinguishing core of [`resolve_instrument`]: `Ok(Ok)` is a built child, `Ok(Err)` is an
/// unavailable source (a `resolve_text` failure, non-fatal) surfaced as the warning
/// so each caller picks its own degradation — the voice pass binds silence (empty graphs), the
/// subpatch pass dissolves the reference dark (nothing spliced in). `slot`
/// names the resource slot the ref came through (`"voice"`/`"patch"`) for the warning. Cycles
/// are refused before resolving: a `source` already on the `loading` stack (a voice/patch chain
/// re-entering itself) is a fatal [`LoadError::CyclicResource`], keyed on the **canonical**
/// source string, so two spellings of one source are one identity.
fn try_resolve_instrument(
    source: &str,
    slot: &'static str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    ctx: &mut LoadCtx,
) -> Result<Result<Loaded, LoadWarning>, LoadError> {
    match resolver.resolve_text(source) {
        Ok(text) => {
            let doc =
                NormalizedDoc::parse_with(&text, registry, Some(resolver), ctx, Some(source))?;
            Ok(Ok(load_child_guarded(
                &doc, source, registry, resolver, ctx,
            )?))
        }
        Err(e) => Ok(Err(LoadWarning::ResolveFailed {
            slot,
            id: source.to_string(),
            source: source.to_string(),
            reason: e.to_string(),
        })),
    }
}

/// Load a child patch's parsed document with the cycle guard: refuse a `source` already on the
/// `loading` stack (a voice/patch chain re-entering itself) as the fatal
/// [`LoadError::CyclicResource`] — keyed on the **canonical** source id —
/// otherwise push it and recurse through [`load_doc_guarded`], with this source as the child's
/// referrer so the child's own references resolve relative to its location.
fn load_child_guarded(
    doc: &NormalizedDoc,
    source: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    ctx: &mut LoadCtx,
) -> Result<Loaded, LoadError> {
    with_cycle_guard(ctx, source, |ctx| {
        load_doc_guarded(doc, registry, resolver, ctx, Some(source))
    })
}

/// Run `f` with `source` pushed on the cycle-guard stack: a chain that
/// re-enters a source still loading — a `voice`/`patch`/re-export chain that (directly or
/// transitively) contains itself — is the fatal [`LoadError::CyclicResource`] instead of
/// infinite recursion. The one statement of the check/push/run/pop discipline, shared by the
/// child-load and migration re-export paths. `source` must already be canonical.
fn with_cycle_guard<T>(
    ctx: &mut LoadCtx,
    source: &str,
    f: impl FnOnce(&mut LoadCtx) -> Result<T, LoadError>,
) -> Result<T, LoadError> {
    if ctx.loading.iter().any(|s| s == source) {
        return Err(LoadError::CyclicResource {
            source: source.to_string(),
        });
    }
    ctx.loading.push(source.to_string());
    let result = f(ctx);
    ctx.loading.pop();
    result
}

impl InstrumentDoc {
    /// Serialize to pretty JSON (the canonical on-disk form).
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("InstrumentDoc serializes")
    }

    /// [`NormalizedDoc::build`] with the nesting machinery threaded through (the
    /// resolution-ordering note): `resolver` loads `subpatch` children so their boundary faces
    /// exist during pass 2 (`None` — the plain [`load`]/[`build`] path — dissolves every nested
    /// reference dark without loading it); `ctx` carries the cycle-guard stack and the decoded-
    /// sample cache shared with [`load_child_guarded`]; `referrer` is this document's canonical
    /// id (`None` at top level), so nested references resolve relative to this document.
    /// Returns the built graph plus availability warnings from nested loads.
    fn build_nested(
        &self,
        registry: &Registry,
        resolver: Option<&dyn ResourceResolver>,
        ctx: &mut LoadCtx,
        referrer: Option<&str>,
    ) -> Result<Loaded, LoadError> {
        let mut graph = Graph::new();
        let mut warnings = Vec::new();
        // No anonymous-`outputs` re-check here: every route to this build starts from a
        // [`NormalizedDoc`], whose mint migrated the block away (v1) or refused it under a
        // v2+ stamp (`check_pipe_shape`) — the invariant is carried by the type.
        debug_assert!(
            self.outputs.is_empty(),
            "a normalized document carries no anonymous outputs"
        );
        // Surface what migration couldn't carry (parse has no warning channel of its own).
        warnings.extend(self.migration.warnings.iter().cloned());
        // address -> (key, descriptor) for resolving wire-refs and outputs. Document nodes only:
        // spliced subpatch internals are deliberately not wireable — the boundary face is the
        // contract (the namespace scopes OSC reachability, not wiring).
        let mut by_addr: BTreeMap<String, (crate::graph::NodeKey, Descriptor)> = BTreeMap::new();
        // Every claimed address — document nodes *and* spliced subpatch internals — so the
        // duplicate check also catches post-prefix collisions (fatal).
        let mut addresses: BTreeSet<String> = BTreeSet::new();
        // Synthesized boundary face per subpatch address: the owned-string port set
        // pass 2 resolves boundary wires against.
        let mut faces: BTreeMap<String, BoundaryFace> = BTreeMap::new();
        // Subpatch addresses that dissolved dark — child unavailable (a warning) or no
        // resolver on this path. Wires and taps touching them are dropped, so the instrument
        // still loads and the nest plays as silence, like a missing voice patch.
        let mut dark: BTreeSet<String> = BTreeSet::new();
        // The resolved boundary, accumulated through the build: input pipes as they mint below,
        // outputs after pass 2 resolves the graph they feed from. Names migration dropped
        // start dark, so a host reference to one degrades like any other dark
        // boundary port instead of failing `UnknownInput`.
        let mut interface = Interface::default();
        interface
            .dark_inputs
            .extend(self.migration.dark_inputs.iter().cloned());

        // Pass 0 — interface input pipes: each `interface.inputs` entry **mints an
        // address in the flat node namespace** (`in` → `/in`) as a loader-built pass-through
        // node of its declared type. Minting runs before the document's own nodes claim
        // addresses, so a collision is the fatal `DuplicateAddress` either way the clash reads.
        // Internal consumers wire from the pipe with ordinary wire-refs — it is in `by_addr`
        // like any source — and whatever feeds the boundary (a parent edge through the face, a
        // Voicer, external OSC, P3's input master) lands on its single `in` port.
        if let Some(iface) = &self.interface {
            for (name, entry) in &iface.inputs {
                let pipe = entry.pipe().ok_or_else(|| LoadError::InterfacePipe {
                    name: name.clone(),
                    reason: "the target-pointing entry form is v1-only — declare the pipe's \
                             type ({\"type\": …}); a v1 document migrates when parsed via \
                             `NormalizedDoc::from_json`"
                        .to_string(),
                })?;
                let (descriptor, kind) = pipe_descriptor(name, pipe)?;
                let bare_signal = kind == PortKind::Signal && descriptor.inputs[0].meta.is_none();
                let address = format!("/{name}");
                if !addresses.insert(address.clone()) {
                    return Err(LoadError::DuplicateAddress(address));
                }
                let key = graph.add_boxed(&address, Box::new(Pipe::new(kind)), descriptor.clone());
                // An enum pipe's declared default seeds the pipe's latch as a value-override;
                // numeric pipes carry theirs inside the port's own meta.
                if descriptor.inputs[0].enum_meta().is_some() {
                    if let Some(PipeDefault::Symbol(s)) = &pipe.default {
                        graph.set_value(key, "in", &Arg::Str(s.as_str().into()));
                    }
                }
                by_addr.insert(address, (key, descriptor));
                interface.inputs.insert(name.clone(), (key, 0));
                if let Some(ch) = pipe.channel {
                    // A logical input channel binding — honored only when this
                    // graph is played at top level; recorded for the input master (P3). The
                    // index is bounded (`MAX_LOGICAL_CHANNELS`): the derived width sizes real
                    // per-channel buffers, so an unbounded document value would turn a typo
                    // into an allocation of arbitrary size. A structural bound is a load
                    // error, not a degrade (degrade is for reality mismatches,
                    // not broken documents).
                    check_logical_channel(name, ch)?;
                    interface.input_channels.insert(name.clone(), ch);
                } else if referrer.is_none() && bare_signal {
                    // A top-level **bare signal** pipe with no channel binding renders
                    // silence forever: no parent edge exists at top level, and audio never
                    // crosses the OSC boundary. Warn, never fatal. A
                    // pipe with a declared default is a control at rest — no warning — and
                    // a nested/hosted child (`referrer.is_some()`) gets `UnwiredPipe` from
                    // its host when the host leaves it unfed.
                    warnings.push(LoadWarning::UnboundInputPipe { name: name.clone() });
                }
            }
        }

        // Pass 1: nodes, config constants, literal inputs.
        for n in &self.nodes {
            let entry = registry
                .get(&n.type_name)
                .ok_or_else(|| LoadError::UnknownType {
                    address: n.address.clone(),
                    type_name: n.type_name.clone(),
                })?;
            if !addresses.insert(n.address.clone()) {
                return Err(LoadError::DuplicateAddress(n.address.clone()));
            }
            let descriptor = entry.descriptor.clone();
            // A resource ref is only valid on an operator that declares that slot (a
            // `sample` on the sample player, a `voice` on the Voicer). Validate
            // data-driven against the node's refs, so a new slot needs no hand-written arm here.
            for (slot, provided) in n.resource_refs() {
                if provided.is_some() && !descriptor.has_resource(slot) {
                    return Err(LoadError::UnknownResource {
                        node: n.address.clone(),
                        slot: slot.to_string(),
                    });
                }
            }
            if descriptor.has_resource("patch") {
                // A nested-instrument reference: no graph node is created — the
                // subpatch pass below splices the child's nodes in and the reference dissolves.
                // Its `inputs` are boundary values, validated there against the
                // synthesized face rather than this (portless) descriptor. `config` has no
                // boundary surface at all — validate it here against the (constant-less)
                // descriptor so a stray entry fails UnknownConfig, exactly as the schema
                // locks it.
                for name in n.config.keys() {
                    if !descriptor.is_constant(name) {
                        return Err(LoadError::UnknownConfig {
                            node: n.address.clone(),
                            name: name.clone(),
                        });
                    }
                }
                continue;
            }
            let key = graph.add_boxed(&n.address, (entry.make)(), descriptor.clone());
            // Retain the logical resource ids so `from_graph` round-trips the reference on save
            // (the resolved bytes/sub-graphs are bound out-of-band and do not survive the build).
            graph.nodes[key].sample_id = n.sample.clone();
            graph.nodes[key].voice_id = n.voice.clone();

            // `config`: every name must be a declared Constant; apply it at its slot.
            for (name, value) in &n.config {
                if !descriptor.is_constant(name) {
                    return Err(LoadError::UnknownConfig {
                        node: n.address.clone(),
                        name: name.clone(),
                    });
                }
                match value {
                    ConfigValue::Number(v) => graph.set_constant(key, name, &Arg::F32(*v as f32)),
                    ConfigValue::Symbol(s) => {
                        graph.set_constant(key, name, &Arg::Str(s.as_str().into()))
                    }
                }
            }

            // `inputs`: a Constant here is an error; literals apply now, wire-refs in pass 2.
            for (name, value) in &n.inputs {
                if descriptor.is_constant(name) {
                    return Err(LoadError::ConstantInInputs {
                        node: n.address.clone(),
                        name: name.clone(),
                    });
                }
                if let Some(arg) = literal_arg(&descriptor, name, value, &n.address, name)? {
                    graph.set_value(key, name, &arg);
                }
            }

            by_addr.insert(n.address.clone(), (key, descriptor));
        }

        // Subpatch pass (nesting P4): resolve each nested reference, load the child
        // through the full path (its own resources bind and its own nests inline — recursion),
        // then splice it into this graph: internal addresses take this node's address as a prefix,
        // edges are remapped, and the boundary face is synthesized from the child's `interface`.
        // The `subpatch` node itself never materializes — it dissolves into its child's nodes.
        // Structural errors in a resolved child stay fatal; availability failures leave the
        // address dark (see above). The source read + parse is deduped per canonical id (like
        // the sample pass's cache); each node still builds its own child — `Graph` is not
        // `Clone` — so two reuses get disjoint nodes, addresses, and state for free.
        let mut patch_docs: BTreeMap<String, Option<NormalizedDoc>> = BTreeMap::new();
        for n in &self.nodes {
            let nested = registry
                .get(&n.type_name)
                .is_some_and(|e| e.descriptor.has_resource("patch"));
            if !nested {
                continue;
            }
            let Some(id) = &n.patch else {
                // No `patch` key at all: an authoring mistake, not an availability failure —
                // but the node still dissolves dark (keeps the instrument playable).
                // Warn loudly: pre-inline this shape was a fatal UnknownPort, and pure silence
                // would hide the typo.
                warnings.push(LoadWarning::NoPatchRef {
                    node: n.address.clone(),
                });
                dark.insert(n.address.clone());
                continue;
            };
            let Some(resolver) = resolver else {
                // No resolver — the plain [`load`]/[`build`] path resolves no resources: every
                // nested reference dissolves dark by design, no warning.
                dark.insert(n.address.clone());
                continue;
            };
            let Some(source) = lookup_source(self, &n.address, "patch", id, &mut warnings) else {
                dark.insert(n.address.clone());
                continue;
            };
            // Canonical id keys the fetch/parse dedup and the cycle guard: two
            // ids spelling one source (`a.json` vs `./a.json`) share one parse and one identity.
            let canon = resolver.canonical(source, referrer);
            if !patch_docs.contains_key(&canon) {
                // A child this load already parsed (a v1 re-export entry migrated through it,
                // or another document referenced it) comes from the load-wide cache; parse
                // failures stay per-document so each keeps its own warning.
                let child_doc = if let Some(cached) = ctx.docs.get(&canon) {
                    Some(cached.clone())
                } else {
                    match resolver.resolve_text(&canon) {
                        Ok(text) => {
                            let parsed = NormalizedDoc::parse_with(
                                &text,
                                registry,
                                Some(resolver),
                                ctx,
                                Some(&canon),
                            )?;
                            ctx.docs.insert(canon.clone(), parsed.clone());
                            Some(parsed)
                        }
                        Err(e) => {
                            let failed = LoadWarning::ResolveFailed {
                                slot: "patch",
                                id: id.clone(),
                                source: canon.clone(),
                                reason: e.to_string(),
                            };
                            warnings.push(failed.nested_in(&n.address));
                            None
                        }
                    }
                };
                patch_docs.insert(canon.clone(), child_doc);
            }
            let Some(child_doc) = &patch_docs[&canon] else {
                dark.insert(n.address.clone());
                continue;
            };
            let loaded = load_child_guarded(child_doc, &canon, registry, resolver, ctx)?;
            warnings.extend(loaded.warnings.into_iter().map(|w| w.nested_in(&n.address)));
            // A subpatch-inlined child's channel bindings are discarded at splice — inert
            //, exactly as under a Voicer, and warned symmetrically: the same
            // authoring slip must not go quiet just because the host is a subpatch.
            for pipe in loaded.graph.interface.input_channels.keys() {
                warnings.push(
                    LoadWarning::InertChannelBinding { name: pipe.clone() }.nested_in(&n.address),
                );
            }
            let face = splice_subpatch(&mut graph, loaded.graph, &n.address, &mut addresses)?;

            // Boundary literals (`"wet": 0.3`): validated against the face and the
            // inner port the interface names, then applied as that inner node's value-override.
            // Errors speak in boundary terms — the subpatch address and external port name —
            // never the prefixed internal address.
            for (name, value) in &n.inputs {
                if matches!(value, InputValue::Wire { .. }) {
                    continue; // pass 2
                }
                let Some(fp) = face.input(name) else {
                    if face.dark_inputs.contains(name) {
                        continue; // dark boundary port: the literal is dropped
                    }
                    return Err(LoadError::UnknownInput {
                        node: n.address.clone(),
                        input: name.clone(),
                    });
                };
                // Pass 1's literal rules, checked against the *inner* port the interface names —
                // the same `literal_arg` statement, labeled in boundary terms.
                let d = &graph.nodes[fp.node].descriptor;
                let inner_name = d.inputs[fp.port].name;
                if let Some(arg) = literal_arg(d, inner_name, value, &n.address, name)? {
                    graph.set_value(fp.node, inner_name, &arg);
                }
            }
            // An unfed **bare signal** input pipe renders silence — warn, never
            // fatal. A pipe with a declared default is a control at rest (no warning), and
            // message pipes are silent-by-nature streams.
            for fp in &face.inputs {
                let p = &graph.nodes[fp.node].descriptor.inputs[fp.port];
                let bare_signal = p.ty.is_buffer() && p.meta.is_none();
                if bare_signal && !n.inputs.contains_key(&fp.name) {
                    warnings.push(LoadWarning::UnwiredPipe {
                        node: n.address.clone(),
                        name: fp.name.clone(),
                    });
                }
            }
            faces.insert(n.address.clone(), face);
        }

        // Pass 2: wire-refs -> edges (Arg-type-checked). A subpatch endpoint resolves through its
        // synthesized boundary face to the inner `(node, port)` its interface
        // names — the same check that guards every other wire covers boundary wires, and the edge
        // lands directly on the inner target, so the splice introduces no untyped edge. Because
        // the face carries the inner port's type verbatim, checking against the face *is*
        // checking against the inner port. Errors name the boundary port (`/reverb.wet`), never
        // the prefixed internal address.
        for n in &self.nodes {
            if dark.contains(&n.address) {
                continue; // unavailable nest: its boundary wires are dropped with the warning
            }
            for (name, value) in &n.inputs {
                let InputValue::Wire { from } = value else {
                    continue;
                };
                // Destination: this node's input port, or — on a subpatch — its face input.
                // A dark boundary port drops the wire, like a dark nest.
                let Some((dst_key, dst_port, to_ty)) =
                    resolve_input(&faces, &by_addr, &n.address, name)?
                else {
                    continue;
                };
                // Source: a node's output port, or a subpatch's face output.
                let (src_addr, src_port_name) = parse_wire(from);
                if dark.contains(src_addr) {
                    continue; // wire from an unavailable nest: dropped (the input keeps its default)
                }
                let Some((src_key, src_port, from_ty, from_label)) =
                    resolve_output(&faces, &by_addr, src_addr, from, src_port_name)?
                else {
                    continue;
                };

                // Equal types wire directly. An `F32` source into a `Buffer` port is the **one
                // implicit bridge** — Value→Signal, ZOH-materialized at the sink. The
                // reverse, a `Buffer` source into an `F32` control port, is Signal→Value: a hard
                // error with no implicit sample-and-hold (an explicit sig→val converter
                // op is the sanctioned path). It is rejected *here*, not left to the plan's form
                // check, so a mistyped wire into a nested boundary fails at load named in boundary
                // terms (`/sub.audio`) instead of surfacing at instantiate as a FormMismatch on
                // the prefixed internals. A type-agnostic `Arg` pass-through input
                // (issue #141) is **capability-keyed**: it accepts any source whose type has an
                // external OSC form (`boundary::has_osc_form`, the single statement shared with
                // the plan check) — the primitives, a vocab enum, `Note`'s registered flat form.
                // A `Buffer` never emits Messages (audio stays off the wire) and
                // `Harmony` has no OSC form (it registers no converter; its wire form is issue
                // #209) — a wire that could never send anything is rejected here, not left
                // silently dead. Anything else is illegal.
                let compatible = same_wire_type(&from_ty, &to_ty)
                    || matches!((&from_ty, &to_ty), (PortType::F32, PortType::F32Buffer))
                    // Numeric widening: an `I32` source into an `F32`/`F32Buffer` sink
                    // is lossless and total (every int is a distinct float; the read coerces via
                    // `Arg::as_f32`), and both are the numeric wiring class — so it widens
                    // implicitly, the same directional favour as `F32→F32Buffer` above. The
                    // reverse, `F32→I32`, is lossy (it needs a rounding decision) and stays a hard
                    // error: an explicit quantizer op is the sanctioned path, mirroring the
                    // `Signal→Value` rule. The four that name the decision ship —
                    // `round`/`floor`/`ceil`/`trunc`'s `*_f32_i32_value` converters — so refusing
                    // here sends the author to a choice, not to a dead end.
                    || matches!(
                        (&from_ty, &to_ty),
                        (PortType::I32 { .. }, PortType::F32)
                            | (PortType::I32 { .. }, PortType::F32Buffer)
                    )
                    || (matches!(to_ty, PortType::Arg) && crate::boundary::has_osc_form(&from_ty));
                if !compatible {
                    return Err(LoadError::TypeMismatch {
                        from: from_label,
                        from_type: Box::new(from_ty),
                        to: format!("{}.{}", n.address, name),
                        to_type: Box::new(to_ty),
                    });
                }
                graph.connect(src_key, src_port, dst_key, dst_port);
            }
        }

        // `interface.outputs`: each named output pipe is **fed from an internal
        // port** — an ordinary wire-ref, resolving through a subpatch face when it names one
        // (re-export) — and, when Signal-typed, feeds the **master taps**: `channel: k` pins
        // logical master channel k (`tap_output_channel`), omitted keeps the broadcast meaning
        //. One consolidated block: at top level it is the master output; nested or
        // hosted, the same entries are the boundary a parent/Voicer reads. An entry whose
        // feeding port is dark — an unavailable subpatch, or a dark boundary port of a live one
        // — is dropped **and recorded** on the resolved interface's dark sets, with a warning,
        // so a consumer one level up degrades the same way instead of hitting a fatal
        // unknown-port error (dark degradation is transitive).
        if let Some(iface) = &self.interface {
            for (name, entry) in &iface.outputs {
                let feed = entry.feed().ok_or_else(|| LoadError::InterfacePipe {
                    name: name.clone(),
                    reason: "the target-pointing entry form is v1-only — feed the pipe from an \
                             internal port ({\"from\": …}); a v1 document migrates when parsed \
                             via `NormalizedDoc::from_json`"
                        .to_string(),
                })?;
                let (src_addr, port) = parse_wire(&feed.from);
                let resolved = if dark.contains(src_addr) {
                    None
                } else {
                    resolve_output(&faces, &by_addr, src_addr, &feed.from, port)?
                };
                let Some((key, idx, ty, _)) = resolved else {
                    interface.dark_outputs.insert(name.clone());
                    warnings.push(LoadWarning::DarkInterfaceEntry {
                        name: name.clone(),
                        target: feed.from.clone(),
                    });
                    continue;
                };
                // Presentational min/max stay a truthful subset of the feeding port's
                // engine-enforced range (the subset law, outputs half).
                check_range_override(
                    name,
                    feed.min,
                    feed.max,
                    &graph.nodes[key].descriptor.outputs[idx],
                    None,
                )?;
                let signal = ty.is_buffer();
                match feed.channel {
                    Some(_) if !signal => {
                        return Err(LoadError::InterfacePipe {
                            name: name.clone(),
                            reason: format!(
                                "`channel` binds hardware channels, which carry signals — \
                                 {:?} feeds from a message-domain port",
                                feed.from
                            ),
                        });
                    }
                    Some(ch) => {
                        // Same structural bound as the input side: the logical output width
                        // sizes real per-channel master buffers.
                        check_logical_channel(name, ch)?;
                        graph.tap_output_channel(key, idx, ch);
                        interface.output_channels.insert(name.clone(), ch);
                    }
                    // A signal pipe with no binding keeps today's broadcast master meaning.
                    None if signal => graph.tap_output(key, idx),
                    // A message-typed pipe (a voice's `active`) is boundary-only — no tap.
                    None => {}
                }
                interface.outputs.insert(name.clone(), (key, idx));
            }
        }
        graph.interface = interface;

        Ok(Loaded { graph, warnings })
    }

    /// The flatten half of [`NormalizedDoc::from_graph`] — the one public export path, which
    /// routes this (current-shaped-by-construction) document through the mint. Kept here so the
    /// flatten stays next to its inverse ([`pipe_doc_from_descriptor`] / [`pipe_descriptor`]).
    fn from_graph_doc(graph: &Graph, instrument: impl Into<String>) -> Self {
        // Interface pipes are loader-built nodes, not document nodes. The graph's
        // **own** boundary pipes (named by `graph.interface`) re-emit as `interface.inputs`
        // entries below; a pipe spliced in from a nested child (its boundary flattened away)
        // **dissolves**: consumers rewire to whatever fed it, or take its seed as a literal.
        let own_pipe: BTreeSet<crate::graph::NodeKey> =
            graph.interface.inputs.values().map(|(k, _)| *k).collect();
        let is_pipe = |k: crate::graph::NodeKey| graph.nodes[k].descriptor.type_name == "pipe";
        // What feeds a dissolved pipe, if anything: its single inbound edge.
        let feeder = |k: crate::graph::NodeKey| {
            graph
                .connections
                .iter()
                .find(|c| c.dst == k)
                .map(|c| (c.src, c.src_port))
        };
        // A pipe's seed value in document form: its author/host override, else its declared
        // numeric default — what an unfed pipe forwards, so a dissolved unfed pipe leaves it
        // on the consumer as a literal.
        let pipe_seed = |k: crate::graph::NodeKey| -> Option<InputValue> {
            let n = &graph.nodes[k];
            let p = &n.descriptor.inputs[0];
            if let Some((_, arg)) = n.value_overrides.first() {
                return Some(match doc_value(p, arg) {
                    DocValue::Number(v) => InputValue::Number(v),
                    DocValue::Symbol(s) => InputValue::Symbol(s),
                });
            }
            p.meta
                .as_ref()
                .map(|m| InputValue::Number(widen_f32(m.default)))
        };

        let mut nodes: Vec<NodeDoc> = graph
            .nodes
            .iter()
            .filter(|(key, _)| !is_pipe(*key))
            .map(|(key, node)| {
                let d = &node.descriptor;
                let mut config: BTreeMap<String, ConfigValue> = BTreeMap::new();
                let mut inputs: BTreeMap<String, InputValue> = BTreeMap::new();

                // Constant overrides go to `config` — a non-default plan-time value the
                // author set (e.g. `voices`). An `i32` count saves as a number; defaults are omitted
                // (they reload from the descriptor), keeping save minimal and round-trips stable.
                for (slot, arg) in &node.constant_overrides {
                    let p = &d.constants[*slot];
                    let value = match doc_value(p, arg) {
                        DocValue::Number(n) => ConfigValue::Number(n),
                        DocValue::Symbol(s) => ConfigValue::Symbol(s),
                    };
                    config.insert(p.name.to_string(), value);
                }
                // Settable input overrides round-trip under the input's name: an `F32`
                // control as a number, an enum as its variant **symbol** (the primary wire form).
                for (port, arg) in &node.value_overrides {
                    let p = &d.inputs[*port];
                    let value = match doc_value(p, arg) {
                        DocValue::Number(n) => InputValue::Number(n),
                        DocValue::Symbol(s) => InputValue::Symbol(s),
                    };
                    inputs.insert(p.name.to_string(), value);
                }
                // Inbound wires: each edge whose destination is this node becomes a wire-ref,
                // using the sole-output sugar when the source has a single output. A source
                // that is a **dissolved** pipe (a flattened nested boundary) is chased to
                // whatever fed it — or, unfed, leaves its seed as a literal.
                for c in graph.connections.iter().filter(|c| c.dst == key) {
                    let (mut src_key, mut src_port) = (c.src, c.src_port);
                    let input_name = d.inputs[c.dst_port].name.to_string();
                    let mut dead = false;
                    while is_pipe(src_key) && !own_pipe.contains(&src_key) {
                        match feeder(src_key) {
                            Some((k, p)) => (src_key, src_port) = (k, p),
                            None => {
                                // Unfed dissolved pipe: its seed becomes the consumer's
                                // literal (nothing, for a bare/message pipe — silence).
                                if let Some(v) = pipe_seed(src_key) {
                                    inputs.insert(input_name.clone(), v);
                                }
                                dead = true;
                                break;
                            }
                        }
                    }
                    if dead {
                        continue;
                    }
                    let src = &graph.nodes[src_key];
                    let from = if src.descriptor.outputs.len() == 1 {
                        src.address.clone()
                    } else {
                        format!("{}.{}", src.address, src.descriptor.outputs[src_port].name)
                    };
                    inputs.insert(input_name, InputValue::Wire { from });
                }

                NodeDoc {
                    type_name: d.type_name.to_string(),
                    address: node.address.clone(),
                    doc: None,
                    config,
                    inputs,
                    // Logical resource ids round-trip from the ids stashed at build (`sample`,
                    // `voice`). The decoded bytes/sub-graphs are bound
                    // out-of-band and are *not* reconstructed here — reload re-resolves from the id.
                    // No `patch`: a subpatch dissolves at build, so a built graph
                    // holds only the flattened equivalent — this save emits the inlined child
                    // nodes, not the reference. Reference-preserving save is the library thread
                    // (P7, #122); the *document*-level round-trip keeps `patch` via serde.
                    sample: node.sample_id.clone(),
                    voice: node.voice_id.clone(),
                    patch: None,
                    // Control metadata lives on the document, not the built Graph, so the
                    // save-from-graph path does not reconstruct it; document-level round-trip
                    // (load → re-serialize) preserves it via serde.
                    control: None,
                }
            })
            .collect();
        nodes.sort_by(|a, b| a.address.cmp(&b.address));

        // Reconstruct the boundary in v2 pipe form. Inputs re-derive each pipe's
        // declaration from its synthesized descriptor; outputs re-emit the canonical explicit
        // `/node.port` feed (never the sole-output sugar) plus its channel binding, so a
        // load → save → reload round-trip is stable. Presentational fields (label/unit/widget)
        // live on the *document*, like `control` — the built Graph doesn't hold them, so this
        // path can't reconstruct them; the document-level round-trip preserves them via serde.
        let iface = &graph.interface;
        let out_ref = |(key, port): &(crate::graph::NodeKey, usize)| {
            let n = &graph.nodes[*key];
            format!("{}.{}", n.address, n.descriptor.outputs[*port].name)
        };
        let inputs: BTreeMap<String, InterfaceEntry> = iface
            .inputs
            .iter()
            .map(|(name, (key, _))| {
                let n = &graph.nodes[*key];
                (
                    name.clone(),
                    InterfaceEntry::Pipe(pipe_doc_from_descriptor(
                        n,
                        iface.input_channels.get(name).copied(),
                    )),
                )
            })
            .collect();
        let mut outputs: BTreeMap<String, InterfaceEntry> = BTreeMap::new();
        // Master taps this boundary already accounts for (a signal output pipe *is* a tap):
        // multiset by (node, port, channel), consumed as interface entries claim them.
        let mut open_taps: Vec<(crate::graph::NodeKey, usize, Option<usize>)> =
            graph.outputs.clone();
        for (name, np @ (key, port)) in &iface.outputs {
            let channel = iface.output_channels.get(name).copied();
            if let Some(i) = open_taps.iter().position(|t| *t == (*key, *port, channel)) {
                open_taps.swap_remove(i);
            }
            outputs.insert(
                name.clone(),
                InterfaceEntry::Feed(OutputPipeDoc {
                    from: out_ref(np),
                    channel,
                    ..Default::default()
                }),
            );
        }
        // Any tap the boundary does not yet name (a programmatically built graph that called
        // `tap_output*` directly) gets a generated entry, exactly as v1 migration names an
        // anonymous tap — so no master output is lost in the flatten.
        for (key, port, channel) in open_taps {
            let name = generated_name(|c| outputs.contains_key(c));
            outputs.insert(
                name,
                InterfaceEntry::Feed(OutputPipeDoc {
                    from: out_ref(&(key, port)),
                    channel,
                    ..Default::default()
                }),
            );
        }
        let interface = if inputs.is_empty() && outputs.is_empty() {
            None
        } else {
            Some(InterfaceDoc { inputs, outputs })
        };

        Self {
            format_version: FORMAT_VERSION,
            instrument: instrument.into(),
            doc: None,
            resources: BTreeMap::new(),
            interface,
            nodes,
            // v2 never writes the anonymous block: every tap is an
            // `interface.outputs` pipe above.
            outputs: Vec::new(),
            migration: MigrationNotes::default(),
        }
    }
}

/// Re-derive an input pipe's document declaration from its synthesized runtime descriptor —
/// [`from_graph`](NormalizedDoc::from_graph)'s inverse of [`pipe_descriptor`]. Range fields are
/// emitted only when they differ from the loader's defaults (the type-wide sentinels, a
/// zero default, a linear curve), so a save is minimal and `build → from_graph` round-trips
/// stably. A host/author value-override on the pipe becomes its declared `default`.
fn pipe_doc_from_descriptor(node: &crate::graph::Node, channel: Option<usize>) -> InputPipeDoc {
    let p = &node.descriptor.inputs[0];
    let mut pipe = InputPipeDoc {
        // No-pipe-form fallback to the raw type string: unreachable for loader-built pipes,
        // harmless for a hand-built graph.
        ty: pipe_type_name(&p.ty).unwrap_or_else(|| p.ty.to_string()),
        channel,
        ..Default::default()
    };
    if pipe.ty == "note" || pipe.ty == "harmony" || pipe.ty == "pitch" {
        return pipe;
    }
    if let PortType::Vocab {
        enum_meta: Some(e), ..
    } = &p.ty
    {
        if let Some((_, arg)) = node.value_overrides.first() {
            if let Some(sym) = e.symbol_of(arg) {
                pipe.default = Some(PipeDefault::Symbol(sym.to_string()));
            }
        }
        return pipe;
    }
    // An integer pipe carries its range/default in `PortType::I32`, not the F32Meta slot
    // — reconstruct min/max/default from there, mirroring the f32 arm below.
    if let PortType::I32 { meta: Some(m) } = &p.ty {
        if m.min != i32::MIN {
            pipe.min = Some(m.min as f64);
        }
        if m.max != i32::MAX {
            pipe.max = Some(m.max as f64);
        }
        let seed = node
            .value_overrides
            .first()
            .and_then(|(_, a)| a.as_f32())
            .map(|v| v.round() as i32)
            .unwrap_or(m.default);
        if seed != 0.clamp(m.min, m.max) {
            pipe.default = Some(PipeDefault::Number(seed as f64));
        }
        return pipe;
    }
    if let Some(m) = &p.meta {
        if m.min != reuben_contract::NUMBER_MIN {
            pipe.min = Some(widen_f32(m.min));
        }
        if m.max != reuben_contract::NUMBER_MAX {
            pipe.max = Some(widen_f32(m.max));
        }
        if m.curve == Curve::Exponential {
            pipe.curve = Some(CurveDoc::Exp);
        }
        let seed = node
            .value_overrides
            .first()
            .and_then(|(_, a)| a.as_f32())
            .unwrap_or(m.default);
        if seed != 0.0_f32.clamp(m.min, m.max) {
            pipe.default = Some(PipeDefault::Number(widen_f32(seed)));
        }
    }
    pipe
}

/// A fresh generated `interface.outputs` entry name (`out`, `out_2`, …): v1-tap migration and
/// the [`from_graph`](NormalizedDoc::from_graph) flatten name anonymous taps through the same
/// generator, so the two paths cannot drift.
fn generated_name(taken: impl Fn(&str) -> bool) -> String {
    let mut i = 0usize;
    loop {
        i += 1;
        let candidate = if i == 1 {
            "out".to_string()
        } else {
            format!("out_{i}")
        };
        if !taken(&candidate) {
            return candidate;
        }
    }
}

/// The pipe **type name** a [`PortType`] declares as, when the type has a pipe form
/// — the one forward mapping shared by v1 migration ([`pipe_from_port`]) and the
/// `from_graph` flatten ([`pipe_doc_from_descriptor`]); [`pipe_descriptor`] owns the reverse
/// (name → synthesized ports) and its arms must mirror this list.
fn pipe_type_name(ty: &PortType) -> Option<String> {
    match ty {
        PortType::F32Buffer => Some("f32_buffer".to_string()),
        PortType::F32 => Some("f32".to_string()),
        PortType::I32 { .. } => Some("i32".to_string()),
        PortType::Vocab {
            name: "Note",
            is_event: true,
            ..
        } => Some("note".to_string()),
        PortType::Vocab {
            name: "Harmony", ..
        } => Some("harmony".to_string()),
        PortType::Vocab { name: "Pitch", .. } => Some("pitch".to_string()),
        PortType::Vocab {
            enum_meta: Some(e), ..
        } => Some(e.type_name.to_string()),
        _ => None,
    }
}

fn lookup<'a>(
    by_addr: &'a BTreeMap<String, (crate::graph::NodeKey, Descriptor)>,
    node: &str,
) -> Result<(crate::graph::NodeKey, &'a Descriptor), LoadError> {
    by_addr
        .get(node)
        .map(|(k, d)| (*k, d))
        .ok_or_else(|| LoadError::UnknownNode(node.to_string()))
}

/// The synthesized boundary face of a `subpatch` node: one port per `interface`
/// name of the resolved child, each carrying the **inner** port's [`PortType`] verbatim (type
/// inherited, never overridable) and the inlined `(node, port)` it resolves to. An owned-string
/// artifact computed at build — deliberately *not* the engine [`Descriptor`], whose names are
/// `&'static str` because operators are compile-time-registered. It exists only
/// long enough to resolve + type-check the parent's boundary wires and then drops with the build
/// scope; the runtime holds no synthesized descriptor at all (what keeps the "zero runtime cost"
/// honest).
struct BoundaryFace {
    /// External input name → inner `(node, input port)` + its type, name-sorted (BTreeMap order).
    inputs: Vec<FacePort>,
    /// External output name → inner `(node, output port)` + its type, name-sorted.
    outputs: Vec<FacePort>,
    /// Declared input names whose target went dark inside the child (see
    /// [`Interface::dark_inputs`]): real boundary ports this load can't resolve. A parent
    /// reference to one degrades (dropped, warned) instead of failing — transitive darkness.
    dark_inputs: BTreeSet<String>,
    /// Declared output names whose target went dark inside the child (see `dark_inputs`).
    dark_outputs: BTreeSet<String>,
}

/// One synthesized boundary port: the external name and the inlined internal target it stands for.
struct FacePort {
    name: String,
    ty: PortType,
    node: crate::graph::NodeKey,
    port: usize,
}

impl BoundaryFace {
    /// The face input named `name`, if the child's `interface` exposes one.
    fn input(&self, name: &str) -> Option<&FacePort> {
        self.inputs.iter().find(|p| p.name == name)
    }

    /// Resolve a face output like [`resolve_out_port`] does for a descriptor, plus the dark
    /// dimension a descriptor doesn't have: `Ok(None)` means the reference lands on a **dark**
    /// boundary port (declared by the child, unresolvable this load) — the caller drops it
    /// instead of erroring. The sole-output sugar counts dark ports as real, so
    /// darkness can only ever drop a wire, never silently re-target it: a two-output face with
    /// one port dark keeps `"/sub"` ambiguous, exactly as if the child were healthy.
    /// `node`/`reference` label errors in the author's terms.
    fn output(
        &self,
        node: &str,
        reference: &str,
        port_name: Option<&str>,
    ) -> Result<Option<&FacePort>, LoadError> {
        if let Some(p) = port_name {
            if self.dark_outputs.contains(p) {
                return Ok(None);
            }
        } else if self.outputs.is_empty() && self.dark_outputs.len() == 1 {
            return Ok(None); // the face's sole output is dark: the sugar resolves to it, darkly
        }
        pick_output(
            |p| self.outputs.iter().position(|o| o.name == p),
            self.outputs.len() + self.dark_outputs.len(),
            node,
            reference,
            port_name,
        )
        .map(|idx| Some(&self.outputs[idx]))
    }
}

/// Validate one literal `inputs` value against the port named `port_name` on `desc` and produce
/// the [`Arg`] to set — `None` for a wire-ref (pass 2's job). The one statement of the literal
/// rules for both surfaces that accept literals — a document node's input in pass 1, and a
/// subpatch boundary input checked against the **inner** port its face names:
/// a number needs a materialized `Float` or an enum, a symbol needs an enum, and the symbol must
/// name a variant (an unknown symbol is an error, never a silent default).
/// `err_node`/`err_input` label errors in the author's terms — for a boundary literal, the
/// subpatch address and external name, never the prefixed internal.
fn literal_arg(
    desc: &Descriptor,
    port_name: &str,
    value: &InputValue,
    err_node: &str,
    err_input: &str,
) -> Result<Option<Arg>, LoadError> {
    match value {
        InputValue::Wire { .. } => Ok(None),
        InputValue::Number(v) => {
            // One predicate for all three settable kinds — a materialized `f32` control, an
            // `i32` control, an enum by index — because it is derived from `Port::coerce`, the
            // conversion this literal is about to go through. Two hand-kept checks here
            // (`materialized_input` + `enum_input`) missed integer ports entirely: the first
            // reads the `F32Meta` slot, and an `i32` port's meta is not there (issue #569).
            if !desc.accepts_number_literal(port_name) {
                return Err(LoadError::UnknownInput {
                    node: err_node.to_string(),
                    input: err_input.to_string(),
                });
            }
            Ok(Some(Arg::F32(*v as f32)))
        }
        InputValue::Symbol(s) => {
            let Some((_, e)) = desc.enum_input(port_name) else {
                return Err(LoadError::UnknownInput {
                    node: err_node.to_string(),
                    input: err_input.to_string(),
                });
            };
            if e.resolve(s).is_none() {
                return Err(LoadError::BadInputValue {
                    node: err_node.to_string(),
                    input: err_input.to_string(),
                    value: s.clone(),
                });
            }
            Ok(Some(Arg::Str(s.as_str().into())))
        }
    }
}

/// Resolve a wire/tap/interface endpoint's **input** side to the inner `(node, port, type)`:
/// through the subpatch's synthesized boundary face when `addr` names one, else
/// through the document node's descriptor. The one statement of face-vs-descriptor input
/// resolution — every pass that lands on an input goes through here, so errors are labeled the
/// same way everywhere: the address and port name the author wrote, never a prefixed internal.
/// `Ok(None)` means the name is a **dark** boundary port (declared by the child, unresolvable
/// this load) — the caller drops the reference; an unknown name stays fatal.
fn resolve_input(
    faces: &BTreeMap<String, BoundaryFace>,
    by_addr: &BTreeMap<String, (crate::graph::NodeKey, Descriptor)>,
    addr: &str,
    name: &str,
) -> Result<Option<(crate::graph::NodeKey, usize, PortType)>, LoadError> {
    match faces.get(addr) {
        Some(face) => match face.input(name) {
            Some(fp) => Ok(Some((fp.node, fp.port, fp.ty.clone()))),
            None if face.dark_inputs.contains(name) => Ok(None),
            None => Err(LoadError::UnknownPort {
                node: addr.to_string(),
                port: name.to_string(),
            }),
        },
        None => {
            let (key, desc) = lookup(by_addr, addr)?;
            let port = in_port(desc, addr, name)?;
            Ok(Some((key, port, desc.inputs[port].ty.clone())))
        }
    }
}

/// [`resolve_input`]'s output-side twin: face output or descriptor output (sole-output sugar in
/// both arms via [`pick_output`]), plus the `"addr.port"` label wire-type errors print — the face
/// arm labels with the **boundary** port name (the "errors speak in boundary terms" rule).
/// Errors name `addr` — the node being resolved — in both arms; `Ok(None)` is a dark boundary
/// port (see [`resolve_input`]).
fn resolve_output(
    faces: &BTreeMap<String, BoundaryFace>,
    by_addr: &BTreeMap<String, (crate::graph::NodeKey, Descriptor)>,
    addr: &str,
    reference: &str,
    port: Option<&str>,
) -> Result<Option<(crate::graph::NodeKey, usize, PortType, String)>, LoadError> {
    // A reference that **exactly names a known address** resolves whole, before the last-`.`
    // split: a v1 interface entry name may carry dots (`"my.tone"` was just a map key), and its
    // migrated pipe mints that name verbatim (`/my.tone`) — splitting would misread it as node
    // `/my` port `tone` and fail a document v1 accepted. Exact-match-first is
    // strictly widening: the split form only ever resolved refs the exact match doesn't claim.
    let (addr, port) =
        if port.is_some() && (by_addr.contains_key(reference) || faces.contains_key(reference)) {
            (reference, None)
        } else {
            (addr, port)
        };
    match faces.get(addr) {
        Some(face) => {
            let Some(fp) = face.output(addr, reference, port)? else {
                return Ok(None);
            };
            Ok(Some((
                fp.node,
                fp.port,
                fp.ty.clone(),
                format!("{}.{}", addr, fp.name),
            )))
        }
        None => {
            let (key, desc) = lookup(by_addr, addr)?;
            let p = resolve_out_port(desc, addr, reference, port)?;
            Ok(Some((
                key,
                p,
                desc.outputs[p].ty.clone(),
                format!("{}.{}", addr, desc.outputs[p].name),
            )))
        }
    }
}

/// Inline a resolved subpatch child into `parent`: move every child node in
/// under `prefix` (its address becomes `<prefix><child address>`, compounding for deeper nests),
/// remap the child's edges onto the new keys, and synthesize the boundary face from the child's
/// resolved `interface`. Prefixing is a pure naming transform — edges are `NodeKey`-resolved, so
/// no wiring can break — and per-reuse state isolation is automatic: each call splices a
/// freshly built child, so two reuses share no keys. A post-prefix address already claimed in
/// `addresses` is the fatal [`LoadError::DuplicateAddress`]. The child's master `outputs`
/// taps do **not** cross the boundary — the `interface` is the whole contract; a nested
/// patch feeds the parent only through its boundary outputs.
fn splice_subpatch(
    parent: &mut Graph,
    mut child: Graph,
    prefix: &str,
    addresses: &mut BTreeSet<String>,
) -> Result<BoundaryFace, LoadError> {
    let mut key_map: BTreeMap<crate::graph::NodeKey, crate::graph::NodeKey> = BTreeMap::new();
    // Deterministic splice order: SlotMap iteration is insertion order, which is doc order.
    let child_keys: Vec<crate::graph::NodeKey> = child.nodes.keys().collect();
    for ck in child_keys {
        let mut node = child.nodes.remove(ck).expect("child key just enumerated");
        let address = format!("{prefix}{}", node.address);
        if !addresses.insert(address.clone()) {
            return Err(LoadError::DuplicateAddress(address));
        }
        node.address = address;
        key_map.insert(ck, parent.nodes.insert(node));
    }
    for c in &child.connections {
        parent.connections.push(crate::graph::Connection {
            src: key_map[&c.src],
            src_port: c.src_port,
            dst: key_map[&c.dst],
            dst_port: c.dst_port,
        });
    }

    // Synthesize the face: type inherited verbatim from the inner port the interface names.
    let face_port = |(name, (ck, port)): (&String, &(crate::graph::NodeKey, usize)),
                     output: bool| {
        let node = key_map[ck];
        let d = &parent.nodes[node].descriptor;
        let ty = if output {
            d.outputs[*port].ty.clone()
        } else {
            d.inputs[*port].ty.clone()
        };
        FacePort {
            name: name.clone(),
            ty,
            node,
            port: *port,
        }
    };
    Ok(BoundaryFace {
        inputs: child
            .interface
            .inputs
            .iter()
            .map(|e| face_port(e, false))
            .collect(),
        outputs: child
            .interface
            .outputs
            .iter()
            .map(|e| face_port(e, true))
            .collect(),
        // Boundary ports the child declared but couldn't resolve (a dark grandchild) stay
        // visible as dark — a parent reference degrades instead of failing (transitivity).
        dark_inputs: child.interface.dark_inputs,
        dark_outputs: child.interface.dark_outputs,
    })
}

/// Widen an `f32` to `f64` without exposing binary-fraction noise: round-trip through the `f32`'s
/// own shortest decimal so `0.2_f32` widens to `0.2`, not `0.20000000298…` (the naive `as f64`).
pub fn widen_f32(v: f32) -> f64 {
    v.to_string().parse().unwrap_or(v as f64)
}

/// A single override [`Arg`] in document-facing form: a number for `F32`/`I32`, the
/// variant **symbol** for an enum choice (the primary wire form).
#[derive(Debug, Clone, PartialEq)]
pub enum DocValue {
    Number(f64),
    Symbol(String),
}

/// The one Arg→document-value mapping, shared by [`NormalizedDoc::from_graph`] (save: `config`
/// and `inputs` overrides) and boundary introspection ([`crate::describe`]) — a new numeric
/// [`Arg`] variant extends this match and nothing downstream. `port` supplies the enum metadata
/// a non-numeric Arg resolves its symbol through.
pub fn doc_value(port: &crate::descriptor::Port, arg: &Arg) -> DocValue {
    match arg {
        Arg::F32(v) => DocValue::Number(widen_f32(*v)),
        Arg::I32(v) => DocValue::Number(*v as f64),
        other => DocValue::Symbol(
            port.enum_meta()
                .and_then(|e| e.symbol_of(other))
                .unwrap_or_default()
                .to_string(),
        ),
    }
}

/// Enforce the presentational-override law: a `min`/`max` override decorates
/// presentation but must stay **truthful**, because `describe` publishes it as the boundary
/// contract and no engine path reconciles it with the range the engine actually clamps to. It
/// must land on a port with a numeric range, stay within the engine-enforced bounds, and not
/// invert; `effective` (inputs only — v1 migration) additionally pins the effective default
/// inside the advertised range. `label`/`unit`/`widget` are unconstrained — they rename, they
/// cannot lie about a value the engine will accept. In v2 this law governs `interface.outputs`
/// overrides and v1 input entries at migration; a v2 **input pipe owns its range outright**
///, validated in [`pipe_descriptor`] instead.
fn check_range_override(
    name: &str,
    min_o: Option<f64>,
    max_o: Option<f64>,
    p: &Port,
    effective: Option<Option<f64>>,
) -> Result<(), LoadError> {
    if min_o.is_none() && max_o.is_none() {
        return Ok(());
    }
    let err = |reason: String| LoadError::InterfaceOverride {
        name: name.to_string(),
        reason,
    };
    // The engine-enforced range: a swept scalar's F32Meta, or an integer port's I32 meta.
    let (inner_min, inner_max) = match (&p.meta, &p.ty) {
        (Some(m), _) => (widen_f32(m.min), widen_f32(m.max)),
        (None, PortType::I32 { meta: Some(m) }) => (m.min as f64, m.max as f64),
        _ => {
            return Err(err(format!(
                "range override on inner port {:?}, which has no numeric range",
                p.name
            )))
        }
    };
    for (bound, value) in [("min", min_o), ("max", max_o)] {
        if let Some(v) = value {
            if v < inner_min || v > inner_max {
                return Err(err(format!(
                    "{bound} {v} is outside the engine-enforced range [{inner_min}..{inner_max}] \
                     — the advertised range must be a subset of what the engine accepts"
                )));
            }
        }
    }
    let lo = min_o.unwrap_or(inner_min);
    let hi = max_o.unwrap_or(inner_max);
    if lo >= hi {
        return Err(err(format!(
            "advertised range [{lo}..{hi}] is inverted or empty"
        )));
    }
    // Inputs only: the effective default is what an unwired host actually gets — it must sit
    // inside the range a generated control will span.
    if let Some(Some(v)) = effective {
        if v < lo || v > hi {
            return Err(err(format!(
                "effective default {v} is outside the advertised range [{lo}..{hi}]"
            )));
        }
    }
    Ok(())
}

/// Synthesize an input pipe's per-entry [`Descriptor`] from its declaration: one
/// `in` port the boundary feeds and one `out` port consumers wire from, both of the **declared**
/// `Arg` type — the existing pass-2 wire check then enforces that type against every consumer,
/// no new checker. A numeric pipe's declared `default`/`min`/`max`/`curve` become the port's own
/// engine-enforced [`F32Meta`] (an unwired signal pipe materializes `default`; a **bare** signal
/// pipe materializes silence). Validation is local and pointed: unknown type, numeric metadata
/// on a message pipe, an incoherent range, or a `channel` on anything but a signal pipe
/// (hardware channels carry signals).
/// Upper bound (exclusive) on a logical `channel` binding, input or output.
/// Logical widths derive as `max bound channel + 1` and size **real per-channel buffers**
/// (the engine's input staging, the render master), so an unbounded document value would turn
/// a typo'd `"channel": 100000000` into a multi-gigabyte allocation. 4096 is beyond any real
/// rig; past it the document is structurally broken, which is a load error, not a degrade
/// (degrades *reality* mismatches, not broken documents).
pub const MAX_LOGICAL_CHANNELS: usize = 4096;

/// Reject an out-of-range logical `channel` binding (see [`MAX_LOGICAL_CHANNELS`]).
fn check_logical_channel(name: &str, ch: usize) -> Result<(), LoadError> {
    if ch >= MAX_LOGICAL_CHANNELS {
        return Err(LoadError::InterfacePipe {
            name: name.to_string(),
            reason: format!(
                "`channel` {ch} is out of range — logical channels are 0..{MAX_LOGICAL_CHANNELS} \
                 (the derived width sizes real per-channel buffers)"
            ),
        });
    }
    Ok(())
}

fn pipe_descriptor(name: &str, pipe: &InputPipeDoc) -> Result<(Descriptor, PortKind), LoadError> {
    let err = |reason: String| LoadError::InterfacePipe {
        name: name.to_string(),
        reason,
    };

    // The declared numeric meta for the pipe's two ports (they mirror each other).
    let f32_meta = || -> Result<F32Meta, LoadError> {
        let min = pipe
            .min
            .map(|v| v as f32)
            .unwrap_or(reuben_contract::NUMBER_MIN);
        let max = pipe
            .max
            .map(|v| v as f32)
            .unwrap_or(reuben_contract::NUMBER_MAX);
        if min >= max {
            return Err(err(format!(
                "declared range [{min}..{max}] is inverted or empty"
            )));
        }
        let default = match &pipe.default {
            None => 0.0_f32.clamp(min, max),
            Some(PipeDefault::Number(v)) => {
                let v = *v as f32;
                if v < min || v > max {
                    return Err(err(format!(
                        "default {v} is outside the declared range [{min}..{max}]"
                    )));
                }
                v
            }
            Some(PipeDefault::Symbol(s)) => {
                return Err(err(format!(
                    "default {s:?} is a symbol — a numeric pipe takes a number"
                )))
            }
        };
        Ok(F32Meta {
            min,
            max,
            default,
            // `unit` is presentational and document-owned; describe reads it from the entry.
            unit: String::new(),
            curve: pipe.curve.map(Curve::from).unwrap_or(Curve::Linear),
        })
    };
    // The declared integer meta for an `i32` pipe's two ports. Mirrors `f32_meta`,
    // but the bounds and default must be whole numbers: a fractional literal on an integer
    // control is an authoring mistake, caught here rather than silently rounded.
    let i32_meta = || -> Result<I32Meta, LoadError> {
        let whole = |v: f64, what: &str| -> Result<i32, LoadError> {
            if v.fract() != 0.0 {
                return Err(err(format!(
                    "{what} {v} is not a whole number — an i32 pipe takes integer bounds/default"
                )));
            }
            Ok(v as i32)
        };
        let min = pipe
            .min
            .map(|v| whole(v, "min"))
            .transpose()?
            .unwrap_or(i32::MIN);
        let max = pipe
            .max
            .map(|v| whole(v, "max"))
            .transpose()?
            .unwrap_or(i32::MAX);
        if min >= max {
            return Err(err(format!(
                "declared range [{min}..{max}] is inverted or empty"
            )));
        }
        if pipe.curve.is_some() {
            return Err(err(
                "`curve` applies to swept f32 pipes only — an integer count has no response curve"
                    .to_string(),
            ));
        }
        let default = match &pipe.default {
            None => 0.clamp(min, max),
            Some(PipeDefault::Number(v)) => {
                let d = whole(*v, "default")?;
                if d < min || d > max {
                    return Err(err(format!(
                        "default {d} is outside the declared range [{min}..{max}]"
                    )));
                }
                d
            }
            Some(PipeDefault::Symbol(s)) => {
                return Err(err(format!(
                    "default {s:?} is a symbol — a numeric pipe takes a number"
                )))
            }
        };
        Ok(I32Meta { min, max, default })
    };
    let no_numeric_meta = || -> Result<(), LoadError> {
        if pipe.default.is_some()
            || pipe.min.is_some()
            || pipe.max.is_some()
            || pipe.curve.is_some()
        {
            return Err(err(format!(
                "`default`/`min`/`max`/`curve` apply to numeric pipes only, not {:?}",
                pipe.ty
            )));
        }
        Ok(())
    };

    let (input, output, kind) = match pipe.ty.as_str() {
        "f32_buffer" => {
            let declared = pipe.default.is_some()
                || pipe.min.is_some()
                || pipe.max.is_some()
                || pipe.curve.is_some();
            if declared {
                let meta = f32_meta()?;
                (
                    Port::f32_buffer_meta("in", meta.clone()),
                    Port::f32_buffer_meta("out", meta),
                    PortKind::Signal,
                )
            } else {
                // A bare signal pipe: unwired it materializes silence (the promise).
                (
                    Port::f32_buffer("in"),
                    Port::f32_buffer("out"),
                    PortKind::Signal,
                )
            }
        }
        "f32" => {
            let meta = f32_meta()?;
            (
                Port::f32("in", meta.clone()),
                Port::f32("out", meta),
                PortKind::Value,
            )
        }
        "i32" => {
            let meta = i32_meta()?;
            (
                Port::i32("in", meta.clone()),
                Port::i32("out", meta),
                PortKind::Value,
            )
        }
        "note" => {
            no_numeric_meta()?;
            (Port::note("in"), Port::note("out"), PortKind::Event)
        }
        "harmony" => {
            no_numeric_meta()?;
            (Port::harmony("in"), Port::harmony("out"), PortKind::Value)
        }
        "pitch" => {
            no_numeric_meta()?;
            (Port::pitch("in"), Port::pitch("out"), PortKind::Value)
        }
        other => {
            let (Some(im), Some(om)) = (
                crate::vocab::enum_meta_by_type(other, "in"),
                crate::vocab::enum_meta_by_type(other, "out"),
            ) else {
                return Err(err(format!(
                    "unknown pipe type {other:?} — one of \"f32_buffer\", \"f32\", \"i32\", \
                     \"note\", \"harmony\", \"pitch\", or a shared vocab enum name (e.g. \"FilterMode\")"
                )));
            };
            if pipe.min.is_some() || pipe.max.is_some() || pipe.curve.is_some() {
                return Err(err(format!(
                    "`min`/`max`/`curve` apply to numeric pipes only, not {other:?}"
                )));
            }
            match &pipe.default {
                Some(PipeDefault::Symbol(s)) if im.resolve(s).is_none() => {
                    return Err(err(format!("default {s:?} names no {other} variant")));
                }
                Some(PipeDefault::Number(_)) => {
                    return Err(err(
                        "an enum pipe's default is its variant symbol, not a number".to_string(),
                    ));
                }
                _ => {}
            }
            (Port::enumerated(im), Port::enumerated(om), PortKind::Value)
        }
    };
    if pipe.channel.is_some() && kind != PortKind::Signal {
        return Err(err(format!(
            "`channel` binds hardware channels, which carry signals — a {:?} pipe is \
             message-domain",
            pipe.ty
        )));
    }
    Ok((
        Descriptor {
            type_name: "pipe",
            inputs: vec![input],
            outputs: vec![output],
            constants: Vec::new(),
            resources: Vec::new(),
        },
        kind,
    ))
}

/// Whether two ports carry the same **`Arg` type** for wiring (the equal-types arm of the
/// pass-2 check). [`PortType`]'s derived equality also compares per-port metadata — a vocab
/// enum's [`EnumMeta`] carries the *port* name, so `pipe.out` (an interface pipe's output) would spuriously mismatch `filter.mode` even though both carry a `FilterMode`.
/// Type identity is the variant plus, for a vocab, its **type** name; metadata (ranges,
/// defaults, port labels) never decides wireability.
fn same_wire_type(from: &PortType, to: &PortType) -> bool {
    match (from, to) {
        (PortType::Vocab { name: a, .. }, PortType::Vocab { name: b, .. }) => a == b,
        _ => std::mem::discriminant(from) == std::mem::discriminant(to),
    }
}

/// Split a wire-ref string into `(node, Some(port))` (`"/osc.audio"`) or `(node, None)` (`"/osc"`,
/// the sole-output sugar). Node addresses carry no `.`, so the last `.` separates node from port.
fn parse_wire(reference: &str) -> (&str, Option<&str>) {
    match reference.rsplit_once('.') {
        Some((node, port)) => (node, Some(port)),
        None => (reference, None),
    }
}

/// Resolve a wire-ref's source output to a port index: the named port, or — under the sole-output
/// sugar — the source's single output (ambiguous, hence an error, if it has several).
fn resolve_out_port(
    desc: &Descriptor,
    node: &str,
    reference: &str,
    port: Option<&str>,
) -> Result<usize, LoadError> {
    pick_output(
        |p| desc.outputs.iter().position(|o| o.name == p),
        desc.outputs.len(),
        node,
        reference,
        port,
    )
}

/// The sole-output sugar, stated once for descriptor and face outputs: a named port resolves by
/// `find`; no name resolves to the single output when there is exactly one, is [`AmbiguousWire`]
/// with several — and with **none** (a face may expose zero outputs) is [`UnknownPort`], because
/// "source has multiple outputs" would be a lie. `node`/`reference` label errors in the author's
/// terms.
fn pick_output(
    find: impl Fn(&str) -> Option<usize>,
    count: usize,
    node: &str,
    reference: &str,
    port: Option<&str>,
) -> Result<usize, LoadError> {
    match port {
        Some(p) => find(p).ok_or_else(|| LoadError::UnknownPort {
            node: node.to_string(),
            port: p.to_string(),
        }),
        None if count == 1 => Ok(0),
        None if count == 0 => Err(LoadError::UnknownPort {
            node: node.to_string(),
            port: reference.to_string(),
        }),
        None => Err(LoadError::AmbiguousWire {
            node: node.to_string(),
            reference: reference.to_string(),
        }),
    }
}

fn in_port(desc: &Descriptor, node: &str, name: &str) -> Result<usize, LoadError> {
    desc.inputs
        .iter()
        .position(|p| p.name == name)
        .ok_or_else(|| LoadError::UnknownPort {
            node: node.to_string(),
            port: name.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO_NODE: &str = r#"
    {
      "instrument": "test",
      "nodes": [
        { "type": "oscillator", "address": "/osc", "inputs": { "freq": 220.0 } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/osc.audio" } } }
      ],
      "outputs": [ {"node":"/out","port":"audio"} ]
    }"#;

    fn reg() -> Registry {
        Registry::builtin()
    }

    #[test]
    fn loads_a_simple_instrument() {
        let g = load(TWO_NODE, &reg()).expect("load");
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.connections.len(), 1);
        assert_eq!(g.outputs.len(), 1);
    }

    #[test]
    fn sole_output_sugar_resolves() {
        // `"/osc"` (no port) is the sole-output sugar — oscillator has one output, `audio`.
        let json = r#"{"instrument":"t","nodes":[
            {"type":"oscillator","address":"/osc"},
            {"type":"output","address":"/out","inputs":{"audio":{"from":"/osc"}}}],
            "outputs":[{"node":"/out","port":"audio"}]}"#;
        let g = load(json, &reg()).expect("load");
        assert_eq!(g.connections.len(), 1);
    }

    #[test]
    fn voices_in_config_sizes_the_voice_pool() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"voicer","address":"/v","config":{"voices":3}}]}"#;
        let g = load(json, &reg()).expect("load");
        let key = g.find("/v").unwrap();
        let slot = g.nodes[key].descriptor.constant_index("voices").unwrap();
        assert_eq!(g.nodes[key].constant_overrides, vec![(slot, Arg::I32(3))]);
    }

    /// An out-of-range `voices` config passes through the single coercion seam
    /// (`Graph::set_constant` → `Port::coerce`), so the **stored** constant clamps to the
    /// voicer's declared `1..=32` range at both ends — a save/reload never resurrects the
    /// out-of-range value. And the pool the voice-resource pass *builds* agrees with the stored
    /// constant: `voice_count` reads the coerced override, not the raw config number, so
    /// `voices: 100` can't build a 100-voice pool while the document stores 32 (a load→save→
    /// reload round trip preserves polyphony).
    #[test]
    fn out_of_range_voices_config_is_clamped_through_the_coercion_seam() {
        let over = r#"{"instrument":"t","nodes":[
            {"type":"voicer","address":"/v","config":{"voices":100}}]}"#;
        let g = load(over, &reg()).expect("load");
        let key = g.find("/v").unwrap();
        let slot = g.nodes[key].descriptor.constant_index("voices").unwrap();
        assert_eq!(g.nodes[key].constant_overrides, vec![(slot, Arg::I32(32))]);
        assert_eq!(voice_count(&g.nodes[key]), 32);

        // Low end: 0 clamps to the I32Meta min (1), not merely a loader-side floor.
        let under = r#"{"instrument":"t","nodes":[
            {"type":"voicer","address":"/v","config":{"voices":0}}]}"#;
        let g = load(under, &reg()).expect("load");
        let key = g.find("/v").unwrap();
        assert_eq!(g.nodes[key].constant_overrides, vec![(slot, Arg::I32(1))]);
        assert_eq!(voice_count(&g.nodes[key]), 1);

        // No config at all: nothing stored, so the pool falls back to the constant's
        // descriptor default — the same value a saved document (which stores no override)
        // rebuilds with.
        let bare = r#"{"instrument":"t","nodes":[
            {"type":"voicer","address":"/v"}]}"#;
        let g = load(bare, &reg()).expect("load");
        let key = g.find("/v").unwrap();
        assert!(g.nodes[key].constant_overrides.is_empty());
        let default = match &g.nodes[key].descriptor.constants[slot].ty {
            PortType::I32 { meta: Some(m) } => m.default as usize,
            other => panic!("voices constant should be I32 with meta, got {other:?}"),
        };
        assert_eq!(voice_count(&g.nodes[key]), default);
    }

    #[test]
    fn enum_symbol_input_loads() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"filter","address":"/f","inputs":{"mode":"Hp"}}]}"#;
        assert!(load(json, &reg()).is_ok());
    }

    #[test]
    fn unknown_type_errors() {
        let json = r#"{"instrument":"t","nodes":[{"type":"nope","address":"/x"}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::UnknownType { .. })
        ));
    }

    #[test]
    fn duplicate_address_errors() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"output","address":"/x"},
            {"type":"output","address":"/x"}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::DuplicateAddress(_))
        ));
    }

    #[test]
    fn unknown_port_errors() {
        let json = r#"{"instrument":"t",
            "nodes":[{"type":"output","address":"/a"},
                     {"type":"output","address":"/b","inputs":{"audio":{"from":"/a.nope"}}}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::UnknownPort { .. })
        ));
    }

    #[test]
    fn unknown_input_errors() {
        let json = r#"{"instrument":"t",
            "nodes":[{"type":"filter","address":"/f","inputs":{"nope":1.0}}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::UnknownInput { .. })
        ));
    }

    #[test]
    fn type_mismatch_errors() {
        // osc.audio is a Buffer output; voicer.notes is a Note input.
        let json = r#"{"instrument":"t",
            "nodes":[{"type":"oscillator","address":"/osc"},
                     {"type":"voicer","address":"/v","inputs":{"notes":{"from":"/osc.audio"}}}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::TypeMismatch { .. })
        ));
    }

    /// The type-agnostic `Arg` pass-through (issue #141): `osc_out.in` accepts any Message-domain
    /// source — a Value `f32` (a Good Button `map` echo) and a `Note` stream both wire in — but a
    /// `Buffer` (audio) source is still a TypeMismatch (audio never crosses the boundary).
    #[test]
    fn arg_passthrough_accepts_message_domain_sources_but_not_audio() {
        // Value f32 source (map_f32_value.out) → osc_out.in: legal.
        let value_src = r#"{"instrument":"t",
            "nodes":[{"type":"map_f32_value","address":"/map"},
                     {"type":"osc_out","address":"/fb","inputs":{"in":{"from":"/map.out"}}}]}"#;
        assert!(load(value_src, &reg()).is_ok());

        // Note event source (sequencer.degrees) → osc_out.in: legal.
        let note_src = r#"{"instrument":"t",
            "nodes":[{"type":"sequencer","address":"/seq"},
                     {"type":"osc_out","address":"/fb","inputs":{"in":{"from":"/seq.degrees"}}}]}"#;
        assert!(load(note_src, &reg()).is_ok());

        // Buffer (audio) source → osc_out.in: rejected.
        let audio_src = r#"{"instrument":"t",
            "nodes":[{"type":"oscillator","address":"/osc"},
                     {"type":"osc_out","address":"/fb","inputs":{"in":{"from":"/osc.audio"}}}]}"#;
        assert!(matches!(
            load(audio_src, &reg()),
            Err(LoadError::TypeMismatch { .. })
        ));
    }

    /// Legality into the pass-through is capability-keyed (`boundary::has_osc_form`): `Harmony`
    /// has no external OSC form (the boundary opt-out — it registers no converter; its wire
    /// form is deferred to issue #209), so the wire could never send anything and is rejected
    /// at load, not left silently dead.
    #[test]
    fn arg_passthrough_rejects_a_source_with_no_osc_form() {
        let harmony_src = r#"{"instrument":"t",
            "nodes":[{"type":"harmony","address":"/h"},
                     {"type":"osc_out","address":"/fb","inputs":{"in":{"from":"/h.harmony"}}}]}"#;
        assert!(matches!(
            load(harmony_src, &reg()),
            Err(LoadError::TypeMismatch { .. })
        ));
    }

    #[test]
    fn unknown_symbol_errors() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"filter","address":"/f","inputs":{"mode":"Nope"}}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::BadInputValue { .. })
        ));
    }

    #[test]
    fn constant_in_inputs_errors() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"voicer","address":"/v","inputs":{"voices":4}}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::ConstantInInputs { .. })
        ));
    }

    #[test]
    fn unknown_config_errors() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"filter","address":"/f","config":{"cutoff":1000}}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::UnknownConfig { .. })
        ));
    }

    #[test]
    fn doc_json_round_trips() {
        let doc = NormalizedDoc::from_json(TWO_NODE, &reg(), None).expect("parse");
        let reparsed =
            NormalizedDoc::from_json(&doc.to_json_pretty(), &reg(), None).expect("reparse");
        assert_eq!(doc, reparsed);
    }

    #[test]
    fn control_block_is_drained_at_parse_and_never_saved() {
        // The retired `control` block parses (deny_unknown_fields would otherwise
        // hard-fail a v2 document) but is drained at normalize — never read, never re-saved.
        let json = r#"{"instrument":"t",
            "nodes":[{"type":"map_f32_signal","address":"/brightness",
                      "control":{"label":"Brightness","widget":"fader","unit":"%"}}]}"#;
        let doc = NormalizedDoc::from_json(json, &reg(), None).expect("parse");
        assert!(
            doc.nodes[0].control.is_none(),
            "normalize drains the retired control block"
        );
        assert!(!doc.to_json_pretty().contains("\"control\""));
    }

    #[test]
    fn absent_format_version_is_v1_and_save_writes_current() {
        // Every pre-versioning document is a valid v1; it migrates at parse...
        let doc = NormalizedDoc::from_json(r#"{"instrument":"t","nodes":[]}"#, &reg(), None)
            .expect("parse");
        assert_eq!(doc.format_version, FORMAT_VERSION);
        // ...and saving stamps the current version explicitly (never the old number).
        let json = doc.to_json_pretty();
        assert!(
            json.contains("\"format_version\": 3"),
            "save must write the current version: {json}"
        );
    }

    #[test]
    fn parse_normalizes_the_version_so_save_never_understamps() {
        // A document below the current version (here the schema-illegal `0`) parses — the gate
        // only refuses the *future* — and normalizes to the current version, so it saves back
        // stamped correctly rather than re-emitting the shape's old, now-wrong version number
        //. Once migrations exist this is what keeps a migrated v1→v2 doc from
        // saving as v1 on a v2 shape.
        let doc = NormalizedDoc::from_json(
            r#"{"format_version":0,"instrument":"t","nodes":[]}"#,
            &reg(),
            None,
        )
        .expect("a below-current version parses");
        assert_eq!(doc.format_version, FORMAT_VERSION);
        assert!(doc.to_json_pretty().contains("\"format_version\": 3"));
    }

    // The old `a_gate_bypassing_doc_is_still_refused_at_load` test is a compile error now —
    // `load_instrument_doc` takes `&NormalizedDoc` by type, so a raw deserialize cannot reach
    // it. Its coverage lives at the gate: `normalize::tests::from_doc_refuses_the_future`.

    #[test]
    fn newer_format_version_is_refused_with_a_clear_error() {
        let err = NormalizedDoc::from_json(
            r#"{"format_version":99,"instrument":"t","nodes":[]}"#,
            &reg(),
            None,
        )
        .expect_err("future version must not parse");
        match &err {
            LoadError::UnsupportedVersion {
                found: 99,
                supported,
            } => {
                assert_eq!(*supported, FORMAT_VERSION);
            }
            other => panic!("expected UnsupportedVersion, got {other:?}"),
        }
        let msg = err.to_string();
        assert!(
            msg.contains("99") && msg.contains("upgrade"),
            "error must name the version and the remedy: {msg}"
        );
    }

    #[test]
    fn newer_format_version_in_a_nested_child_is_fatal() {
        // The gate lives at the parse boundary, so a too-new *child* refuses too — a host
        // must not splice a shape it can't trust (structural, not availability).
        let future_child =
            PatchResolver(r#"{"format_version":99,"instrument":"future","nodes":[]}"#);
        const HOST: &str = r#"{
            "instrument": "host",
            "resources": { "f": "future.json" },
            "nodes": [ { "type": "subpatch", "address": "/f", "patch": "f" } ]
        }"#;
        match load_instrument(HOST, &Registry::builtin(), &future_child) {
            Err(LoadError::UnsupportedVersion { found: 99, .. }) => {}
            Err(other) => panic!("expected UnsupportedVersion, got {other:?}"),
            Ok(_) => panic!("a too-new child must be fatal"),
        }
    }

    #[test]
    fn from_graph_then_build_is_stable() {
        // load -> save -> reparse -> save again: the two saved docs are identical.
        let g1 = load(TWO_NODE, &reg()).expect("load");
        let saved1 = NormalizedDoc::from_graph(&g1, "test", &reg());
        let g2 = saved1.build(&reg()).expect("rebuild");
        let saved2 = NormalizedDoc::from_graph(&g2, "test", &reg());
        assert_eq!(saved1, saved2);
        assert_eq!(saved1.nodes.len(), 2);
    }

    /// Issue #569: a numeric literal must set an **`i32`** input port, not be rejected as an
    /// unknown input. The gate used to ask `materialized_input`, which reads the `F32Meta`
    /// struct field — where an integer port carries its meta *inside* `PortType::I32` — so every
    /// port of the ten `*_i32_value` operators was invisible to it and `"a": 3` failed at load
    /// naming a port that plainly exists.
    ///
    /// The stored override is `Arg::I32`, which is the assertion that matters: the literal went
    /// through the same `Port::coerce` seam a wire or an OSC message does, rather than being
    /// admitted as a raw `F32`.
    #[test]
    fn a_numeric_literal_sets_an_i32_input_port() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"add_i32_value","address":"/k","inputs":{"a":3,"b":4}}]}"#;
        let g = load(json, &reg()).expect("load");
        let key = g.find("/k").unwrap();
        let d = &g.nodes[key].descriptor;
        let a = d.inputs.iter().position(|p| p.name == "a").unwrap();
        let b = d.inputs.iter().position(|p| p.name == "b").unwrap();
        assert_eq!(
            g.nodes[key].value_overrides,
            vec![(a, Arg::I32(3)), (b, Arg::I32(4))]
        );
    }

    /// The literal reaches the integer port through the *same* coercion the runtime uses, so it
    /// rounds and clamps identically — a fractional literal is not an `as i32` truncation, and a
    /// literal past the type-wide `±1e6` sentinel pins to it rather than storing a value the
    /// port would refuse from OSC (issue #569).
    #[test]
    fn an_i32_port_literal_rounds_and_clamps_like_the_runtime() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"add_i32_value","address":"/k","inputs":{"a":3.7,"b":9999999}}]}"#;
        let g = load(json, &reg()).expect("load");
        let key = g.find("/k").unwrap();
        let stored: Vec<_> = g.nodes[key]
            .value_overrides
            .iter()
            .map(|(_, arg)| arg.clone())
            .collect();
        // 3.7 rounds to 4 (not truncated to 3); 9_999_999 clamps to the i32 type-wide max.
        assert_eq!(stored, vec![Arg::I32(4), Arg::I32(1_000_000)]);
    }

    /// The save path writes an `Arg::I32` override back as a plain number, so the reader had to
    /// accept a number on an `i32` port for `load → save → load` to close. Before issue #569 the
    /// two halves disagreed: whatever the writer emitted, the reader refused it. Pinned as a
    /// round trip rather than a save assertion, because it is the *pairing* that was broken.
    #[test]
    fn an_i32_port_literal_survives_a_save_reload_round_trip() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"add_i32_value","address":"/k","inputs":{"a":3,"b":4}}]}"#;
        let g1 = load(json, &reg()).expect("load");
        let saved = NormalizedDoc::from_graph(&g1, "t", &reg());
        assert_eq!(saved.nodes[0].inputs["a"], InputValue::Number(3.0));
        let g2 = saved.build(&reg()).expect("the saved document reloads");
        let key = g2.find("/k").unwrap();
        assert_eq!(
            g2.nodes[key].value_overrides,
            g1.nodes[g1.find("/k").unwrap()].value_overrides
        );
    }

    /// #556 PR 2: a port converted from `f32` to `i32` (here `euclid.steps`) is a **hard** load
    /// error for an `f32`-source wire — `F32 -> I32` narrows and there is no implicit narrowing
    /// (#556 decision 1). The sanctioned path is an `i32` source (or a `round` converter),
    /// proven to load by the sibling case. This is the guard that the contract actually bites
    /// after the conversion — a literal still coerces (rounds + clamps), but a *wire* must carry
    /// the type.
    #[test]
    fn a_converted_i32_port_refuses_an_f32_wire() {
        // `add_f32_value.out` is a Value `F32`; `euclid.steps` is now a Value `i32` → narrowing.
        let f32_src = r#"{"instrument":"t","nodes":[
            {"type":"add_f32_value","address":"/a","inputs":{"a":8.0,"b":8.0}},
            {"type":"euclid","address":"/e","inputs":{"steps":{"from":"/a.out"}}}]}"#;
        assert!(matches!(
            load(f32_src, &reg()),
            Err(LoadError::TypeMismatch { .. })
        ));
        // An `i32` source wires straight in — the sanctioned integer path (`i32 -> i32`).
        let i32_src = r#"{"instrument":"t","nodes":[
            {"type":"add_i32_value","address":"/a","inputs":{"a":8,"b":8}},
            {"type":"euclid","address":"/e","inputs":{"steps":{"from":"/a.out"}}}]}"#;
        assert!(load(i32_src, &reg()).is_ok());
    }

    /// The widened gate must not start admitting literals on ports that genuinely take none: a
    /// bare audio buffer has no scalar to set, and an unknown name is still unknown (issue #569).
    #[test]
    fn a_literal_on_a_portless_or_bare_buffer_input_is_still_unknown() {
        let bare_buffer = r#"{"instrument":"t","nodes":[
            {"type":"output","address":"/o","inputs":{"audio":1.0}}]}"#;
        assert!(matches!(
            load(bare_buffer, &reg()),
            Err(LoadError::UnknownInput { .. })
        ));
        let nonesuch = r#"{"instrument":"t","nodes":[
            {"type":"add_i32_value","address":"/k","inputs":{"nope":1}}]}"#;
        assert!(matches!(
            load(nonesuch, &reg()),
            Err(LoadError::UnknownInput { .. })
        ));
    }

    #[test]
    fn value_overrides_round_trip_across_types() {
        // The collapsed `value_overrides` channel must save an `F32` control override as a
        // number and an enum override as its variant **symbol** (reconstructed via
        // `EnumMeta::symbol_of`, not a stored index) — one channel, two on-disk forms.
        let json = r#"{"instrument":"t","nodes":[
            {"type":"filter","address":"/f","inputs":{"mode":"Hp","cutoff":3000}}]}"#;
        let g = load(json, &reg()).expect("load");
        let doc = NormalizedDoc::from_graph(&g, "t", &reg());
        let inputs = &doc.nodes[0].inputs;
        assert_eq!(inputs["cutoff"], InputValue::Number(3000.0));
        assert_eq!(inputs["mode"], InputValue::Symbol("Hp".to_string()));
    }

    // The `interface` block. A voice-shaped patch: osc.freq / env.gate in,
    // osc.audio / env.active out. `/env` has two outputs so a sole-output ref would be ambiguous;
    // the explicit `/env.active` resolves it.
    // A well-formed v1 voice: like every shipped v1 voice patch, its `audio` boundary output
    // is ALSO anonymously tapped, so migration claims the entry (one tap, no divergence, no
    // warning — see `boundary_only_v1_signal_outputs_warn_on_migration` for the other case).
    const VOICE_IFACE: &str = r#"{
        "instrument": "voice",
        "interface": {
            "inputs":  { "freq": "/osc.freq", "gate": "/env.gate" },
            "outputs": { "audio": "/osc.audio", "active": "/env.active" }
        },
        "nodes": [
            { "type": "oscillator", "address": "/osc" },
            { "type": "envelope", "address": "/env" }
        ],
        "outputs": [ { "node": "/osc", "port": "audio" } ]
    }"#;

    #[test]
    fn interface_inputs_become_pipes_and_outputs_resolve_to_internal_ports() {
        // Via v1 migration: each input entry mints a pipe node (`freq` → `/freq`)
        // whose `in` port the boundary feeds; the old target consumes it with an ordinary
        // wire; outputs resolve to the internal ports that feed them.
        let g = load(VOICE_IFACE, &reg()).expect("load");
        let osc = g.find("/osc").unwrap();
        let env = g.find("/env").unwrap();
        let freq_pipe = g.find("/freq").expect("input pipe minted at /freq");
        let gate_pipe = g.find("/gate").expect("input pipe minted at /gate");

        // The boundary feeds each pipe's `in` port.
        assert_eq!(g.interface.inputs["freq"], (freq_pipe, 0));
        assert_eq!(g.interface.inputs["gate"], (gate_pipe, 0));
        // The pipe's declared type derives from the old target port (osc.freq is a
        // signal-with-default control; env.gate a held f32 Value).
        assert_eq!(
            g.nodes[freq_pipe].descriptor.inputs[0].ty,
            PortType::F32Buffer
        );
        assert_eq!(g.nodes[gate_pipe].descriptor.inputs[0].ty, PortType::F32);
        // The flip: the old targets now consume the pipes with ordinary edges.
        let osc_freq = g.nodes[osc]
            .descriptor
            .inputs
            .iter()
            .position(|p| p.name == "freq")
            .unwrap();
        assert!(g
            .connections
            .iter()
            .any(|c| c.src == freq_pipe && c.dst == osc && c.dst_port == osc_freq));
        let env_gate = g.nodes[env]
            .descriptor
            .inputs
            .iter()
            .position(|p| p.name == "gate")
            .unwrap();
        assert!(g
            .connections
            .iter()
            .any(|c| c.src == gate_pipe && c.dst == env && c.dst_port == env_gate));

        // Outputs resolve to the right node + output port index (explicit `/env.active`).
        let osc_d = &g.nodes[osc].descriptor;
        let env_d = &g.nodes[env].descriptor;
        assert_eq!(
            g.interface.outputs["audio"],
            (
                osc,
                osc_d
                    .outputs
                    .iter()
                    .position(|p| p.name == "audio")
                    .unwrap()
            )
        );
        assert_eq!(
            g.interface.outputs["active"],
            (
                env,
                env_d
                    .outputs
                    .iter()
                    .position(|p| p.name == "active")
                    .unwrap()
            )
        );
    }

    #[test]
    fn native_v2_pipe_fans_out_to_several_consumers() {
        // A pipe behaves like a source node, so fan-out is free — two consumers
        // wire from one input pipe with plain wire-refs.
        let json = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"in":{"type":"f32_buffer"}}},
            "nodes":[
              {"type":"filter","address":"/a","inputs":{"audio":{"from":"/in"}}},
              {"type":"delay","address":"/b","inputs":{"audio":{"from":"/in"}}}]}"#;
        let g = load(json, &reg()).expect("load");
        let pipe = g.find("/in").expect("pipe minted");
        let consumers = g.connections.iter().filter(|c| c.src == pipe).count();
        assert_eq!(consumers, 2, "one pipe, two consumer edges");
    }

    #[test]
    fn pipe_address_collision_with_a_node_is_fatal() {
        // The entry mints `/in`; a document node claiming the same address is the
        // existing fatal DuplicateAddress.
        let json = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"in":{"type":"f32_buffer"}}},
            "nodes":[{"type":"output","address":"/in"}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::DuplicateAddress(a)) if a == "/in"
        ));
    }

    #[test]
    fn pitch_pipe_type_mints_a_held_pitch_value_port() {
        // A `pitch` interface input pipe is a first-class held-Value pipe, parallel to
        // `harmony`: it mints a `Pitch` Value port and round-trips to the `"pitch"` type name.
        let json = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"p":{"type":"pitch"}}},
            "nodes":[]}"#;
        let g = load(json, &reg()).expect("pitch pipe loads");
        let pipe = g.find("/p").expect("input pipe minted at /p");
        let ty = &g.nodes[pipe].descriptor.inputs[0].ty;
        assert!(
            matches!(
                ty,
                PortType::Vocab {
                    name: "Pitch",
                    is_event: false,
                    ..
                }
            ),
            "{ty:?}"
        );
        // The reverse mapping mirrors the forward parser (round-trip).
        assert_eq!(pipe_type_name(ty), Some("pitch".to_string()));
    }

    #[test]
    fn channel_on_a_message_pipe_is_a_load_error() {
        // Hardware channels carry signals; a channel binding on a Value/Event
        // pipe is a pointed load error, both directions.
        for iface in [
            r#""inputs":{"gate":{"type":"f32","channel":0}}"#,
            r#""inputs":{"notes":{"type":"note","channel":1}}"#,
        ] {
            let json = format!(
                r#"{{"format_version":2,"instrument":"t","interface":{{{iface}}},"nodes":[]}}"#
            );
            let err = load(&json, &reg()).err().expect("channel on message pipe");
            assert!(matches!(&err, LoadError::InterfacePipe { .. }), "{err:?}");
            assert!(err.to_string().contains("channel"), "{err}");
        }
        // The output direction: `active` is a Value (message) port.
        let json = r#"{"format_version":2,"instrument":"t","interface":{
            "outputs":{"act":{"from":"/env.active","channel":0}}},
            "nodes":[{"type":"envelope","address":"/env"}]}"#;
        let err = load(json, &reg()).err().expect("channel on message feed");
        assert!(matches!(&err, LoadError::InterfacePipe { .. }), "{err:?}");
    }

    #[test]
    fn channel_bindings_derive_the_logical_widths() {
        // Input width = max bound input channel + 1 (0 when none); output pipes
        // with channels feed the pinned master taps.
        let json = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"mic_l":{"type":"f32_buffer","channel":0},
                      "mic_r":{"type":"f32_buffer","channel":3}},
            "outputs":{"main_l":{"from":"/gain.out","channel":0}}},
            "nodes":[{"type":"mul_f32_signal","address":"/gain","inputs":{"a":{"from":"/mic_l"}}}]}"#;
        let g = load(json, &reg()).expect("load");
        let width = |g: &crate::graph::Graph| {
            g.interface
                .input_channels
                .values()
                .map(|&c| c + 1)
                .max()
                .unwrap_or(0)
        };
        assert_eq!(width(&g), 4, "max bound input channel + 1");
        assert_eq!(g.interface.input_channels["mic_r"], 3);
        assert_eq!(g.outputs.len(), 1);
        assert_eq!(g.outputs[0].2, Some(0), "output pipe channel pins the tap");
        // A patch that binds no input channel pays nothing.
        let none = load(VOICE_IFACE, &reg()).expect("load");
        assert_eq!(width(&none), 0);
    }

    #[test]
    fn unknown_pipe_type_is_a_pointed_error() {
        let json = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"in":{"type":"f32buffer"}}},"nodes":[]}"#;
        let err = load(json, &reg()).err().expect("unknown type");
        assert!(err.to_string().contains("unknown pipe type"), "{err}");
    }

    /// An `i32` interface pipe synthesizes an integer port carrying its range + default in
    /// [`PortType::I32`]'s [`I32Meta`] — parallel to an `f32` pipe's F32Meta.
    #[test]
    fn int_pipe_parses_range_and_default() {
        let json = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"steps":{"type":"i32","min":1,"max":16,"default":8,"unit":"steps"}}},
            "nodes":[]}"#;
        let g = load(json, &reg()).expect("load");
        let key = g.find("/steps").expect("pipe node minted");
        assert!(
            matches!(
                &g.nodes[key].descriptor.inputs[0].ty,
                PortType::I32 { meta: Some(m) } if *m == I32Meta { min: 1, max: 16, default: 8 }
            ),
            "got {:?}",
            g.nodes[key].descriptor.inputs[0].ty
        );
    }

    /// An `i32` control pipe wires into an operator's `f32` port
    /// (euclid's `steps`) — the lossless numeric widening — and loads clean, no converter op.
    #[test]
    fn int_pipe_widens_into_a_float_control() {
        let json = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"steps":{"type":"i32","min":1,"max":16,"default":8}}},
            "nodes":[
              {"type":"clock","address":"/clk"},
              {"type":"euclid","address":"/eu","inputs":{
                 "clock":{"from":"/clk.gate"},"steps":{"from":"/steps"}}}]}"#;
        let g = load(json, &reg()).expect("i32 pipe must widen into euclid's f32 steps");
        let eu = g.find("/eu").unwrap();
        let steps = g.nodes[eu]
            .descriptor
            .inputs
            .iter()
            .position(|p| p.name == "steps")
            .unwrap();
        assert!(
            g.connections
                .iter()
                .any(|c| c.dst == eu && c.dst_port == steps),
            "euclid.steps must be wired from the int pipe"
        );
    }

    /// The **reverse** crossing, and the reason the rounding family exists. `f32 -> i32` is lossy
    /// — it needs a rounding *decision* — so the wire check refuses it outright and the sanctioned
    /// path is an operator that names which decision (issue #556, `per-wire-form-check`).
    ///
    /// Both halves are pinned together deliberately: a test that only asserted the rejection would
    /// still pass if the crossing were simply impossible, which is the state this replaced.
    #[test]
    fn a_float_source_reaches_an_int_port_only_through_a_converter() {
        let direct = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"amount":{"type":"f32","min":0,"max":16,"default":4}}},
            "nodes":[
              {"type":"add_i32_value","address":"/n","inputs":{"a":{"from":"/amount"}}}]}"#;
        assert!(
            matches!(load(direct, &reg()), Err(LoadError::TypeMismatch { .. })),
            "a bare f32 -> i32 wire must stay a hard error"
        );

        let converted = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"amount":{"type":"f32","min":0,"max":16,"default":4}}},
            "nodes":[
              {"type":"round_f32_i32_value","address":"/q","inputs":{"x":{"from":"/amount"}}},
              {"type":"add_i32_value","address":"/n","inputs":{"a":{"from":"/q.out"}}}]}"#;
        let g = load(converted, &reg()).expect("the converter bridges f32 -> i32");
        let q = g.find("/q").unwrap();
        let n = g.find("/n").unwrap();
        assert!(
            g.connections.iter().any(|c| c.src == q && c.dst == n),
            "the converter's i32 output must reach the i32 port"
        );
    }

    /// A fractional default on an integer control is an authoring mistake, refused at load —
    /// not silently rounded (the round is for *runtime* traffic).
    #[test]
    fn int_pipe_rejects_a_fractional_default() {
        let json = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"steps":{"type":"i32","min":1,"max":16,"default":8.5}}},"nodes":[]}"#;
        let err = load(json, &reg()).err().expect("fractional default");
        assert!(err.to_string().contains("whole number"), "{err}");
    }

    /// A count has no response curve — `curve` on an `i32` pipe is refused (I32Meta has no curve).
    #[test]
    fn int_pipe_rejects_a_curve() {
        let json = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"steps":{"type":"i32","min":1,"max":16,"curve":"exp"}}},"nodes":[]}"#;
        let err = load(json, &reg()).err().expect("curve on int");
        assert!(err.to_string().contains("curve"), "{err}");
    }

    /// `i32` and `f32` are **distinct** wire types: the widening is not a
    /// `same_wire_type` equality — it is the one-directional `I32→F32` exception in the
    /// `compatible` gate. So the reverse (`F32→I32`, lossy) has no path here and stays a hard
    /// error, exactly as `Signal→Value` does. Guards against a future edit making it symmetric.
    #[test]
    fn int_and_float_are_distinct_wire_types() {
        assert!(!same_wire_type(
            &PortType::I32 { meta: None },
            &PortType::F32
        ));
        assert!(!same_wire_type(
            &PortType::F32,
            &PortType::I32 { meta: None }
        ));
    }

    #[test]
    fn v2_doc_with_v1_forms_is_refused() {
        // A v2 stamp must not hide v1 shapes: target-form entries and the anonymous outputs
        // block are both refused with pointed errors.
        let target = r#"{"format_version":2,"instrument":"t","interface":{
            "inputs":{"freq":"/osc.freq"}},
            "nodes":[{"type":"oscillator","address":"/osc"}]}"#;
        assert!(matches!(
            load(target, &reg()),
            Err(LoadError::InterfacePipe { name, .. }) if name == "freq"
        ));
        let anon = r#"{"format_version":2,"instrument":"t",
            "nodes":[{"type":"oscillator","address":"/osc"}],
            "outputs":[{"node":"/osc","port":"audio"}]}"#;
        assert!(matches!(
            load(anon, &reg()),
            Err(LoadError::AnonymousOutputs)
        ));
    }

    #[test]
    fn interface_unknown_node_errors() {
        let json = r#"{"instrument":"t","interface":{"inputs":{"freq":"/nope.freq"}},
            "nodes":[{"type":"oscillator","address":"/osc"}]}"#;
        assert!(matches!(load(json, &reg()), Err(LoadError::UnknownNode(_))));
    }

    #[test]
    fn interface_unknown_port_errors() {
        // `/osc` has no input named `gate` — a direction-correct but absent port.
        let json = r#"{"instrument":"t","interface":{"inputs":{"gate":"/osc.gate"}},
            "nodes":[{"type":"oscillator","address":"/osc"}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::UnknownPort { .. })
        ));
    }

    // The object entry form: target + presentational overrides.
    const VOICE_IFACE_META: &str = r#"{
        "instrument": "voice",
        "interface": {
            "inputs": {
                "freq": { "target": "/osc.freq", "label": "Pitch", "unit": "Hz", "min": 50, "max": 2000, "widget": "knob" },
                "gate": "/env.gate"
            },
            "outputs": { "audio": "/osc.audio" }
        },
        "nodes": [
            { "type": "oscillator", "address": "/osc" },
            { "type": "envelope", "address": "/env" }
        ]
    }"#;

    #[test]
    fn v1_object_form_migrates_like_the_bare_form_with_overrides_applied() {
        // The v1 object form's `target` migrates exactly as a bare string entry would; its
        // presentational overrides decorate the derived pipe (label/unit/widget). Its min/max
        // were **presentational** in v1 — the engine clamped to the inner port's range — so
        // the pipe keeps the inner (oscillator freq) engine range: migrating the narrowing
        // into the pipe's now-engine-enforced range would clamp harder than v1 did.
        let g = load(VOICE_IFACE_META, &reg()).expect("load");
        let pipe = g.find("/freq").expect("pipe minted");
        assert_eq!(g.interface.inputs["freq"], (pipe, 0));
        let p = &g.nodes[pipe].descriptor.inputs[0];
        assert_eq!(p.ty, PortType::F32Buffer, "type derived from /osc.freq");
        let m = p.meta.as_ref().expect("numeric pipe meta");
        assert_eq!(
            (m.min, m.max),
            (20.0, 20_000.0),
            "the pipe enforces the inner port's engine range, not the v1 display narrowing"
        );
        assert_eq!(m.default, 440.0, "target's default carried over");
    }

    #[test]
    fn migrated_entry_round_trips_through_the_document_as_a_pipe() {
        // Migration happens at parse, so the held document is v2: the entry is a pipe carrying
        // the v1 overrides as its own declaration, and re-serialize → reparse is stable.
        let doc = NormalizedDoc::from_json(VOICE_IFACE_META, &reg(), None).expect("parse");
        let iface = doc.interface.as_ref().unwrap();
        let pipe = iface.inputs["freq"].pipe().expect("migrated to a pipe");
        assert_eq!(pipe.ty, "f32_buffer");
        // v1's `label`/`widget` are retired presentation: drained on the way to
        // v3 with a DeprecatedPipePresentation warning. The quantity fields carry over.
        assert_eq!(pipe.label, None);
        assert_eq!(pipe.widget, None);
        assert_eq!(pipe.unit.as_deref(), Some("Hz"));
        // v1 min/max were display-only; the pipe carries the inner port's engine range.
        assert_eq!((pipe.min, pipe.max), (Some(20.0), Some(20_000.0)));
        assert_eq!(pipe.default, Some(PipeDefault::Number(440.0)));
        // The consumer flipped: /osc.freq now wires from the pipe.
        let osc = doc.nodes.iter().find(|n| n.address == "/osc").unwrap();
        assert_eq!(
            osc.inputs["freq"],
            InputValue::Wire {
                from: "/freq".to_string()
            }
        );
        let reparsed =
            NormalizedDoc::from_json(&doc.to_json_pretty(), &reg(), None).expect("reparse");
        assert_eq!(doc, reparsed);
    }

    #[test]
    fn v1_entry_mixing_target_and_type_is_rejected() {
        // An object may carry exactly one discriminator (`type`/`from`/`target`) — mixing the
        // v1 target form with a v2 type declaration is a parse error naming the conflict.
        let json = r#"{"instrument":"t","interface":{
            "inputs":{"freq":{"target":"/osc.freq","type":"note"}}},
            "nodes":[{"type":"oscillator","address":"/osc"}]}"#;
        let err = match load(json, &reg()) {
            Err(e @ LoadError::Json(_)) => e,
            Err(e) => panic!("expected Json error, got {e:?}"),
            Ok(_) => panic!("mixed entry must not load"),
        };
        assert!(
            err.to_string().contains("only one of"),
            "error must name the conflict: {err}"
        );
    }

    #[test]
    fn interface_entry_errors_name_the_offending_field() {
        // Hand-dispatched entry parsing (discriminator key, then the concrete struct) keeps
        // pointed serde errors; `#[serde(untagged)]` would collapse all of these into one
        // opaque "did not match any variant" message.
        let entry = |body: &str| {
            format!(
                r#"{{"instrument":"t","interface":{{"inputs":{{"freq":{body}}}}},
                "nodes":[{{"type":"oscillator","address":"/osc"}}]}}"#
            )
        };
        for (body, expect) in [
            (
                r#"{"target":"/osc.freq","lable":"Pitch"}"#,
                "unknown field `lable`",
            ),
            (r#"{"label":"Pitch"}"#, "needs `type`"),
            (
                r#"{"target":"/osc.freq","min":"200"}"#,
                "invalid type: string",
            ),
            (r#"{"type":"f32","chanel":0}"#, "unknown field `chanel`"),
            (r#"true"#, "target string"),
        ] {
            let err = load(&entry(body), &reg())
                .err()
                .unwrap_or_else(|| panic!("{body} must not load"));
            assert!(
                err.to_string().contains(expect),
                "{body}: expected {expect:?} in error, got: {err}"
            );
        }
    }

    /// `load` unwrapped to its error (Graph is not Debug, so no `expect_err`).
    fn load_err(json: &str, why: &str) -> LoadError {
        match load(json, &reg()) {
            Err(e) => e,
            Ok(_) => panic!("{why}"),
        }
    }

    // The override law (review F1/F5/F6): a range override must stay a subset of the
    // engine-enforced range, not invert, land on a numeric port, and keep the effective default
    // inside the advertised range — `describe` publishes it as the boundary contract.
    fn iface_freq(entry: &str, freq_literal: &str) -> String {
        // Oscillator `freq`: engine-enforced [20..20000], descriptor default 440.
        format!(
            r#"{{"instrument":"t","interface":{{"inputs":{{"pitch":{entry}}}}},
            "nodes":[{{"type":"oscillator","address":"/osc"{freq_literal}}}]}}"#
        )
    }

    #[test]
    fn interface_override_narrowing_the_engine_range_loads() {
        let json = iface_freq(r#"{"target":"/osc.freq","min":50,"max":2000}"#, "");
        load(&json, &reg()).expect("narrowing override with in-range default loads");
    }

    #[test]
    fn interface_override_outside_the_engine_range_is_rejected() {
        // Advertising 5 Hz when the engine clamps to 20 would let `describe` publish a range
        // nothing enforces.
        let json = iface_freq(r#"{"target":"/osc.freq","min":5,"max":2000}"#, "");
        let err = load_err(&json, "widened range must not load");
        assert!(
            matches!(&err, LoadError::InterfaceOverride { name, .. } if name == "pitch"),
            "boundary-named: {err}"
        );
        assert!(err.to_string().contains("engine-enforced range"), "{err}");
    }

    #[test]
    fn interface_inverted_range_override_is_rejected() {
        let json = iface_freq(r#"{"target":"/osc.freq","min":8000,"max":200}"#, "");
        let err = load_err(&json, "inverted range must not load");
        assert!(err.to_string().contains("inverted or empty"), "{err}");
    }

    #[test]
    fn interface_override_leaving_the_effective_default_outside_is_rejected() {
        // The child's own literal (3000) is what an unwired host gets — it must sit inside the
        // advertised [50..2000], or a generated control starts out of range.
        let json = iface_freq(
            r#"{"target":"/osc.freq","min":50,"max":2000}"#,
            r#","inputs":{"freq":3000.0}"#,
        );
        let err = load_err(&json, "out-of-range effective default must not load");
        assert!(err.to_string().contains("effective default 3000"), "{err}");

        // Same law with no literal: the descriptor default (440) must fit the advertised range.
        let json = iface_freq(r#"{"target":"/osc.freq","min":500,"max":2000}"#, "");
        let err = load_err(&json, "descriptor default outside range must not load");
        assert!(err.to_string().contains("effective default 440"), "{err}");
    }

    #[test]
    fn interface_range_override_on_a_rangeless_port_is_rejected() {
        // `waveform` is an enum — no numeric range for a min/max to narrow. (Label/unit/widget
        // stay legal anywhere: they rename, they can't lie about accepted values.)
        let json = iface_freq(r#"{"target":"/osc.waveform","min":0}"#, "");
        let err = load_err(&json, "range on rangeless port must not load");
        assert!(err.to_string().contains("no numeric range"), "{err}");
    }

    #[test]
    fn interface_input_requires_explicit_port() {
        // No sole-input sugar: an `inputs` ref must name its port.
        let json = r#"{"instrument":"t","interface":{"inputs":{"freq":"/osc"}},
            "nodes":[{"type":"oscillator","address":"/osc"}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::UnknownPort { .. })
        ));
    }

    #[test]
    fn interface_round_trips_through_doc_and_graph() {
        // Document round-trip (serde) preserves the block...
        let doc = NormalizedDoc::from_json(VOICE_IFACE, &reg(), None).expect("parse");
        let reparsed =
            NormalizedDoc::from_json(&doc.to_json_pretty(), &reg(), None).expect("reparse");
        assert_eq!(doc, reparsed);
        // ...and from_graph reconstructs an equivalent, stable interface: inputs re-derive
        // their pipe declaration, outputs the canonical explicit `/node.port` feed.
        let g = load(VOICE_IFACE, &reg()).expect("load");
        let saved = NormalizedDoc::from_graph(&g, "voice", &reg());
        let iface = saved.interface.as_ref().expect("interface reconstructed");
        assert_eq!(
            iface.inputs["freq"].pipe().expect("pipe form").ty,
            "f32_buffer"
        );
        assert_eq!(
            iface.outputs["active"].feed().expect("feed form").from,
            "/env.active"
        );
        // Rebuild and compare by (address, port) — raw NodeKeys are build-specific (the slotmap
        // assigns them fresh, and from_graph re-sorts nodes), so resolve keys to addresses first.
        let g2 = saved.build(&reg()).expect("rebuild");
        let addr =
            |g: &Graph, (k, p): (crate::graph::NodeKey, usize)| (g.nodes[k].address.clone(), p);
        for name in ["freq", "gate"] {
            assert_eq!(
                addr(&g, g.interface.inputs[name]),
                addr(&g2, g2.interface.inputs[name]),
                "input {name} drifted"
            );
        }
        for name in ["audio", "active"] {
            assert_eq!(
                addr(&g, g.interface.outputs[name]),
                addr(&g2, g2.interface.outputs[name]),
                "output {name} drifted"
            );
        }
    }

    // The instrument-kind resource. A resolver whose `resolve_text` returns a voice
    // patch's JSON; `resolve_instrument` builds it into a sub-Graph (with its interface resolved).
    struct PatchResolver(&'static str);
    impl ResourceResolver for PatchResolver {
        fn resolve(&self, source: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
            Err(crate::resources::ResolveError::NotFound(source.to_string()))
        }
        fn resolve_text(&self, _: &str) -> Result<String, crate::resources::ResolveError> {
            Ok(self.0.to_string())
        }
    }

    #[test]
    fn instrument_resource_resolves_path_to_subgraph() {
        let loaded = resolve_instrument("voices/lead.json", &reg(), &PatchResolver(VOICE_IFACE))
            .expect("resolve");
        assert!(loaded.warnings.is_empty());
        // Two document nodes plus the two minted input pipes.
        assert_eq!(loaded.graph.nodes.len(), 4);
        // The sub-Graph carries its resolved interface, ready for a host to bind.
        assert!(loaded.graph.interface.inputs.contains_key("freq"));
        assert!(loaded.graph.interface.outputs.contains_key("audio"));
    }

    #[test]
    fn instrument_resource_resolve_failure_is_a_warning_not_fatal() {
        // A resolver that can't produce the text (the default `resolve_text`): non-fatal — an empty graph plus a ResolveFailed warning, never a hard error.
        struct Failing;
        impl ResourceResolver for Failing {
            fn resolve(&self, s: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
                Err(crate::resources::ResolveError::NotFound(s.to_string()))
            }
        }
        let loaded = resolve_instrument("missing.json", &reg(), &Failing).expect("non-fatal");
        assert_eq!(loaded.graph.nodes.len(), 0);
        assert!(matches!(
            loaded.warnings.as_slice(),
            [LoadWarning::ResolveFailed { .. }]
        ));
    }

    #[test]
    fn instrument_resource_structural_error_is_fatal() {
        // A sub-patch that resolves but is structurally broken (unknown operator type) is fatal,
        // availability problems warn, wiring problems error.
        const BROKEN: &str = r#"{"instrument":"v","nodes":[{"type":"nope","address":"/x"}]}"#;
        assert!(matches!(
            resolve_instrument("v.json", &reg(), &PatchResolver(BROKEN)),
            Err(LoadError::UnknownType { .. })
        ));
    }

    // Nesting P4 — a `subpatch` node references an instrument patch; at build the
    // child is loaded recursively and **inlined**: nodes spliced under the subpatch's address
    // prefix, boundary wires resolved through the synthesized face, the node dissolved. Reuses
    // the `VOICE_IFACE` patch as the child.
    const PARENT_WITH_SUBPATCH: &str = r#"{
        "instrument": "parent",
        "resources": { "myvoice": "voices/lead.json" },
        "nodes": [
            { "type": "subpatch", "address": "/sub", "patch": "myvoice" }
        ]
    }"#;

    // A parent exercising the whole boundary surface: a literal onto a face input, a wire out of
    // a face output, and a master tap through the face.
    const PARENT_WIRED: &str = r#"{
        "instrument": "parent",
        "resources": { "v": "voices/lead.json" },
        "nodes": [
            { "type": "subpatch", "address": "/sub", "patch": "v",
              "inputs": { "freq": 220.0 } },
            { "type": "output", "address": "/out",
              "inputs": { "audio": { "from": "/sub.audio" } } }
        ],
        "outputs": [ { "node": "/out", "port": "audio" } ]
    }"#;

    #[test]
    fn subpatch_inlines_child_under_prefixed_addresses() {
        // The P4 acceptance shape: the child's nodes are spliced in under the
        // subpatch's address prefix and the subpatch node itself dissolves — no node named
        // `/sub` survives, and the internals stay addressable as first-class parent nodes.
        let loaded = load_instrument(PARENT_WITH_SUBPATCH, &reg(), &PatchResolver(VOICE_IFACE))
            .expect("load");
        assert!(
            loaded.warnings.is_empty(),
            "clean load: {:?}",
            loaded.warnings
        );
        assert!(
            loaded.graph.find("/sub").is_none(),
            "the subpatch node dissolves at build"
        );
        assert!(loaded.graph.find("/sub/osc").is_some());
        assert!(loaded.graph.find("/sub/env").is_some());
        // The child's input pipes splice in under the prefix too.
        assert!(loaded.graph.find("/sub/freq").is_some());
        assert!(loaded.graph.find("/sub/gate").is_some());
        assert_eq!(
            loaded.graph.nodes.len(),
            4,
            "child nodes + its input pipes, no host node"
        );
    }

    #[test]
    fn boundary_wires_and_literals_resolve_through_the_face() {
        // A wire out of `/sub.audio` lands directly on the inner `(node, port)`
        // the child interface names, and a literal onto `/sub.freq` becomes the inner node's
        // value-override — both through the synthesized face, no subpatch node in between.
        let loaded =
            load_instrument(PARENT_WIRED, &reg(), &PatchResolver(VOICE_IFACE)).expect("load");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let g = &loaded.graph;
        let osc = g.find("/sub/osc").expect("inlined child oscillator");
        let out = g.find("/out").expect("parent output node");

        // The boundary wire is one ordinary edge: inner osc.audio -> out.audio.
        let audio_out = g.nodes[osc]
            .descriptor
            .outputs
            .iter()
            .position(|p| p.name == "audio")
            .unwrap();
        assert!(
            g.connections
                .iter()
                .any(|c| c.src == osc && c.src_port == audio_out && c.dst == out),
            "face output rewired to the inner target: {:?}",
            g.connections
        );

        // The boundary literal seeds the child's `freq` PIPE: the pipe forwards it
        // to the oscillator it feeds, so the value still lands — but as pipe state, not an
        // override on the inner node.
        let pipe = g.find("/sub/freq").expect("spliced freq pipe");
        assert!(
            g.nodes[pipe]
                .value_overrides
                .iter()
                .any(|(p, a)| *p == 0 && *a == Arg::F32(220.0)),
            "boundary literal lands on the pipe: {:?}",
            g.nodes[pipe].value_overrides
        );
        let osc_freq = g.nodes[osc]
            .descriptor
            .inputs
            .iter()
            .position(|p| p.name == "freq")
            .unwrap();
        assert!(
            g.connections
                .iter()
                .any(|c| c.src == pipe && c.dst == osc && c.dst_port == osc_freq),
            "the pipe feeds the inner oscillator"
        );

        // The master tap on /out survives untouched.
        assert_eq!(g.outputs.len(), 1);
    }

    #[test]
    fn master_tap_through_the_face_resolves_to_the_inner_output() {
        // An `outputs` tap naming the subpatch resolves through the face to the inner port.
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v"}],
            "outputs":[{"node":"/sub","port":"audio"}]}"#;
        let loaded = load_instrument(json, &reg(), &PatchResolver(VOICE_IFACE)).expect("load");
        let osc = loaded.graph.find("/sub/osc").unwrap();
        assert_eq!(loaded.graph.outputs.len(), 1);
        assert_eq!(loaded.graph.outputs[0].0, osc);
    }

    #[test]
    fn sole_output_sugar_is_ambiguous_on_a_two_output_face() {
        // `"/sub"` with no port: VOICE_IFACE exposes two boundary outputs (audio, active), so the
        // sugar is ambiguous — same rule as a two-output operator, named in boundary terms.
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v"},
            {"type":"output","address":"/out","inputs":{"audio":{"from":"/sub"}}}]}"#;
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver(VOICE_IFACE)),
            Err(LoadError::AmbiguousWire { .. })
        ));
    }

    #[test]
    fn sole_output_sugar_resolves_on_a_single_output_face() {
        const MONO_CHILD: &str = r#"{
            "instrument": "mono",
            "interface": { "outputs": { "audio": "/osc.audio" } },
            "nodes": [ { "type": "oscillator", "address": "/osc" } ]
        }"#;
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v"},
            {"type":"output","address":"/out","inputs":{"audio":{"from":"/sub"}}}]}"#;
        let loaded = load_instrument(json, &reg(), &PatchResolver(MONO_CHILD)).expect("load");
        assert_eq!(loaded.graph.connections.len(), 1);
    }

    #[test]
    fn sole_output_sugar_on_a_zero_output_face_is_unknown_port() {
        // A face may expose no outputs at all — then `"/sub"` with no port is UnknownPort, not
        // AmbiguousWire ("source has multiple outputs" would be a lie).
        const INPUT_ONLY_CHILD: &str = r#"{
            "instrument": "sink",
            "interface": { "inputs": { "freq": "/osc.freq" } },
            "nodes": [ { "type": "oscillator", "address": "/osc" } ]
        }"#;
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v"},
            {"type":"output","address":"/out","inputs":{"audio":{"from":"/sub"}}}]}"#;
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver(INPUT_ONLY_CHILD)),
            Err(LoadError::UnknownPort { node, .. }) if node == "/sub"
        ));
    }

    #[test]
    fn unknown_boundary_port_errors_in_boundary_terms() {
        // A wire into a face input the interface doesn't expose: UnknownPort naming the subpatch
        // address and the external name — never the prefixed internals (P5 hardens this further).
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"oscillator","address":"/osc"},
            {"type":"subpatch","address":"/sub","patch":"v",
             "inputs":{"nope":{"from":"/osc.audio"}}}]}"#;
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver(VOICE_IFACE)),
            Err(LoadError::UnknownPort { node, port }) if node == "/sub" && port == "nope"
        ));
        // A literal onto a missing boundary input follows pass 1's rule: UnknownInput.
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v","inputs":{"nope":1.0}}]}"#;
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver(VOICE_IFACE)),
            Err(LoadError::UnknownInput { node, input }) if node == "/sub" && input == "nope"
        ));
    }

    #[test]
    fn type_mismatch_across_the_boundary_is_fatal() {
        // The face inherits the inner port's type verbatim, so the ordinary pass-2 check
        // rejects an illegal boundary wire: osc.audio (Buffer) into a Note boundary input. The
        // error speaks in boundary terms (`/sub.notes`).
        const NOTE_CHILD: &str = r#"{
            "instrument": "notes",
            "interface": { "inputs": { "notes": "/v.notes" } },
            "nodes": [ { "type": "voicer", "address": "/v" } ]
        }"#;
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"oscillator","address":"/osc"},
            {"type":"subpatch","address":"/sub","patch":"v",
             "inputs":{"notes":{"from":"/osc.audio"}}}]}"#;
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver(NOTE_CHILD)),
            Err(LoadError::TypeMismatch { to, .. }) if to == "/sub.notes"
        ));
    }

    #[test]
    fn buffer_into_f32_value_input_is_fatal_at_load() {
        // Signal→Value is a hard error with no implicit sample-and-hold. The load-time
        // check owns it (not just the plan's form check), so it fails at load everywhere — and
        // the message points at the sanctioned converter path.
        let json = r#"{"instrument":"t","nodes":[
            {"type":"oscillator","address":"/osc"},
            {"type":"add_f32_value","address":"/sum","inputs":{"a":{"from":"/osc.audio"}}}]}"#;
        let Err(err) = load(json, &reg()) else {
            panic!("Buffer -> F32 must fail at load");
        };
        assert!(matches!(
            &err,
            LoadError::TypeMismatch { from, to, .. }
                if from == "/osc.audio" && to == "/sum.a"
        ));
        let msg = err.to_string();
        assert!(
            msg.contains("(F32Buffer)") && msg.contains("(F32)"),
            "{msg}"
        );
        assert!(msg.contains("sample-and-hold"), "{msg}");
    }

    // P5: the adversarial boundary-type matrix. Every case is a **well-typed inner
    // graph** with a mistyped *boundary* wire, and every case must fail at parent load — these
    // tests never reach `Plan::instantiate` — with an error naming the boundary port in the
    // author's terms, never the prefixed internals.

    /// A child whose face spans the port kinds: a bare-`F32` Value input, a `Buffer` audio
    /// output, and a vocab-enum input — the faithfulness matrix's fixture.
    const KINDS_CHILD: &str = r#"{
        "instrument": "kinds",
        "interface": {
            "inputs":  { "gain": "/amt.a", "waveform": "/osc.waveform" },
            "outputs": { "audio": "/osc.audio", "level": "/amt.out" }
        },
        "nodes": [
            { "type": "oscillator", "address": "/osc" },
            { "type": "add_f32_value", "address": "/amt" }
        ]
    }"#;

    #[test]
    fn boundary_f32_input_inherits_the_inner_value_type() {
        // Face input `gain` is the inner add_f32_value's bare-F32 Value port: an audio wire into
        // it is Signal→Value, fatal at load, named `/sub.gain` — with the converter hint.
        let json = r#"{"instrument":"p","resources":{"k":"k.json"},"nodes":[
            {"type":"oscillator","address":"/mod"},
            {"type":"subpatch","address":"/sub","patch":"k",
             "inputs":{"gain":{"from":"/mod.audio"}}}]}"#;
        let Err(err) = load_instrument(json, &reg(), &PatchResolver(KINDS_CHILD)) else {
            panic!("audio into an F32 boundary input must fail at load");
        };
        assert!(matches!(
            &err,
            LoadError::TypeMismatch { from, to, .. }
                if from == "/mod.audio" && to == "/sub.gain"
        ));
        assert!(err.to_string().contains("sample-and-hold"), "{err}");
    }

    #[test]
    fn boundary_enum_input_inherits_the_inner_vocab_type() {
        // Face input `waveform` is the inner oscillator's Waveform enum: an audio wire into it is
        // fatal, and the message prints the concrete vocab name, not a Debug dump of its meta.
        let json = r#"{"instrument":"p","resources":{"k":"k.json"},"nodes":[
            {"type":"oscillator","address":"/mod"},
            {"type":"subpatch","address":"/sub","patch":"k",
             "inputs":{"waveform":{"from":"/mod.audio"}}}]}"#;
        let Err(err) = load_instrument(json, &reg(), &PatchResolver(KINDS_CHILD)) else {
            panic!("audio into an enum boundary input must fail at load");
        };
        assert!(
            matches!(&err, LoadError::TypeMismatch { to, .. } if to == "/sub.waveform"),
            "{err:?}"
        );
        let msg = err.to_string();
        assert!(msg.contains("(Waveform)"), "{msg}");
        assert!(!msg.contains("enum_meta"), "Debug leak: {msg}");
    }

    #[test]
    fn boundary_output_mismatch_names_the_boundary_port_as_source() {
        // The other direction: a boundary *output* (`/sub.audio`, Buffer) wired into a parent
        // F32 Value input. The error's `from` is the boundary label, not `/sub/osc.audio`.
        let json = r#"{"instrument":"p","resources":{"k":"k.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"k"},
            {"type":"add_f32_value","address":"/sum",
             "inputs":{"a":{"from":"/sub.audio"}}}]}"#;
        let Err(err) = load_instrument(json, &reg(), &PatchResolver(KINDS_CHILD)) else {
            panic!("boundary audio into a parent F32 input must fail at load");
        };
        assert!(matches!(
            &err,
            LoadError::TypeMismatch { from, to, .. }
                if from == "/sub.audio" && to == "/sum.a"
        ));
    }

    #[test]
    fn mistyping_only_the_second_of_two_reuses_names_that_reuse() {
        // Two reuses of one child; only `/b`'s wire is mistyped. The error names `/b.gain` —
        // per-reuse identity holds for errors, not just state.
        let json = r#"{"instrument":"p","resources":{"k":"k.json"},"nodes":[
            {"type":"oscillator","address":"/mod"},
            {"type":"subpatch","address":"/a","patch":"k","inputs":{"gain":0.5}},
            {"type":"subpatch","address":"/b","patch":"k",
             "inputs":{"gain":{"from":"/mod.audio"}}}]}"#;
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver(KINDS_CHILD)),
            Err(LoadError::TypeMismatch { to, .. }) if to == "/b.gain"
        ));
    }

    #[test]
    fn nest_in_nest_boundary_mismatch_names_the_outer_face() {
        // A middle patch re-exports its inner child's F32 boundary input; the parent mistypes a
        // wire into the *outer* face. The error speaks the outermost author's terms
        // (`/outer.gain`), leaking neither `/inner.gain` nor `/outer/inner/amt.a`.
        struct Chain;
        impl ResourceResolver for Chain {
            fn resolve(&self, s: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
                Err(crate::resources::ResolveError::NotFound(s.to_string()))
            }
            fn resolve_text(&self, source: &str) -> Result<String, crate::resources::ResolveError> {
                Ok(match source {
                    "mid.json" => r#"{"instrument":"mid",
                            "resources":{"leaf":"leaf.json"},
                            "interface":{"inputs":{"gain":"/inner.gain"},
                                         "outputs":{"audio":"/inner.audio"}},
                            "nodes":[{"type":"subpatch","address":"/inner","patch":"leaf"}]}"#
                        .to_string(),
                    _ => KINDS_CHILD.to_string(),
                })
            }
        }
        let json = r#"{"instrument":"p","resources":{"m":"mid.json"},"nodes":[
            {"type":"oscillator","address":"/mod"},
            {"type":"subpatch","address":"/outer","patch":"m",
             "inputs":{"gain":{"from":"/mod.audio"}}}]}"#;
        let Err(err) = load_instrument(json, &reg(), &Chain) else {
            panic!("mistyped wire into a re-exported boundary port must fail at load");
        };
        assert!(
            matches!(&err, LoadError::TypeMismatch { to, .. } if to == "/outer.gain"),
            "{err:?}"
        );
        assert!(!err.to_string().contains("/inner"), "{err}");
    }

    #[test]
    fn boundary_enum_symbol_literal_reaches_the_inner_port() {
        // A symbol literal on a boundary input resolves against the *inner* enum port; an unknown
        // variant is BadInputValue named at the boundary (never snaps to default).
        const FILTER_CHILD: &str = r#"{
            "instrument": "fx",
            "interface": { "inputs": { "mode": "/f.mode" } },
            "nodes": [ { "type": "filter", "address": "/f" } ]
        }"#;
        let ok = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v","inputs":{"mode":"Hp"}}]}"#;
        let loaded = load_instrument(ok, &reg(), &PatchResolver(FILTER_CHILD)).expect("load");
        let pipe = loaded.graph.find("/sub/mode").expect("spliced enum pipe");
        assert!(
            !loaded.graph.nodes[pipe].value_overrides.is_empty(),
            "symbol literal seeds the enum pipe's override"
        );
        let bad = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v","inputs":{"mode":"Nope"}}]}"#;
        assert!(matches!(
            load_instrument(bad, &reg(), &PatchResolver(FILTER_CHILD)),
            Err(LoadError::BadInputValue { node, input, .. })
                if node == "/sub" && input == "mode"
        ));
    }

    #[test]
    fn unwired_nested_bare_signal_pipe_warns_and_renders_silence() {
        // A nested child's bare signal input pipe left unfed by the host renders
        // silence — honest, but almost always an authoring slip, so it warns. A defaulted
        // control pipe at rest and a fed pipe stay quiet.
        const FX_CHILD: &str = r#"{"format_version":2,"instrument":"fx",
            "interface":{
              "inputs":{"in":{"type":"f32_buffer"},
                        "tone":{"type":"f32_buffer","default":1000,"min":20,"max":20000}},
              "outputs":{"out":{"from":"/f.audio"}}},
            "nodes":[{"type":"filter","address":"/f",
                      "inputs":{"audio":{"from":"/in"},"cutoff":{"from":"/tone"}}}]}"#;
        // Host leaves `in` unfed: one warning, naming the node and the pipe.
        let unfed = r#"{"format_version":2,"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v"}]}"#;
        let loaded = load_instrument(unfed, &reg(), &PatchResolver(FX_CHILD)).expect("load");
        assert!(matches!(
            loaded.warnings.as_slice(),
            [LoadWarning::UnwiredPipe { node, name }] if node == "/sub" && name == "in"
        ));
        // Host wires it: clean.
        let fed = r#"{"format_version":2,"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"oscillator","address":"/osc"},
            {"type":"subpatch","address":"/sub","patch":"v",
             "inputs":{"in":{"from":"/osc.audio"}}}]}"#;
        let loaded = load_instrument(fed, &reg(), &PatchResolver(FX_CHILD)).expect("load");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    }

    #[test]
    fn boundary_only_v1_signal_outputs_warn_on_migration() {
        // The accepted divergence (decided 2026-07-04): a v1 **signal**
        // interface output no anonymous tap claims becomes a real master tap in v2 — audible
        // at top level where v1 played nothing from it — and migration says so, naming the
        // entry. Message-typed entries never tap, so they migrate silently.
        let json = r#"{"instrument":"c","interface":{
            "inputs":{"in":"/f.audio"},
            "outputs":{"out":"/f.audio","state":"/env.active"}},
            "nodes":[{"type":"filter","address":"/f"},{"type":"envelope","address":"/env"}]}"#;
        let loaded = load_instrument(json, &reg(), &PatchResolver("")).expect("load");
        // Two warnings here, one per concern: the migration divergence for `out`, plus the
        // input master's unbound-bare-pipe notice for `in` (this doc binds no channel; that
        // path landed with P3). The Value-typed `state` entry never taps, so it stays silent.
        assert!(
            matches!(
                loaded.warnings.as_slice(),
                [
                    LoadWarning::Migration { name, detail },
                    LoadWarning::UnboundInputPipe { name: pipe }
                ] if name == "out" && detail.contains("master tap") && pipe == "in"
            ),
            "exactly the signal entry warns of the tap, plus the unbound input pipe: {:?}",
            loaded.warnings
        );
    }

    #[test]
    fn voicer_hosted_bare_signal_pipe_warns_unwired() {
        // The silence warning covers "nested/hosted" alike: a Voicer feeds
        // only the message pipes it knows by name (`freq`/`gate`), so a hosted voice's bare
        // signal pipe is never fed — it renders silence and must warn, exactly as the
        // subpatch face does.
        const AUX_VOICE: &str = r#"{"format_version":2,"instrument":"v",
            "interface":{
              "inputs":{"aux":{"type":"f32_buffer"}},
              "outputs":{"audio":{"from":"/out.audio"}}},
            "nodes":[{"type":"output","address":"/out","inputs":{"audio":{"from":"/aux"}}}]}"#;
        let host = r#"{"format_version":2,"instrument":"h","resources":{"v":"v.json"},
            "interface":{"outputs":{"out":{"from":"/voicer.audio"}}},
            "nodes":[{"type":"voicer","address":"/voicer","voice":"v"}]}"#;
        let loaded = load_instrument(host, &reg(), &PatchResolver(AUX_VOICE)).expect("load");
        assert!(
            loaded.warnings.iter().any(|w| matches!(
                w,
                LoadWarning::UnwiredPipe { node, name } if node == "/voicer" && name == "aux"
            )),
            "hosted bare signal pipe must warn: {:?}",
            loaded.warnings
        );
    }

    // The old `a_v2_stamped_doc_handed_to_load_instrument_doc_is_shape_checked` smuggle test
    // (#189 F8a) is a compile error now — neither `load_instrument_doc` nor `build` accepts a
    // raw `InstrumentDoc`, which is the win. Its coverage lives at the one door left:
    // `normalize::tests::from_doc_refuses_v1_forms_under_a_current_stamp`.

    #[test]
    fn post_prefix_address_collision_is_fatal() {
        // A child address that, after prefixing, collides with an existing parent
        // address is a DuplicateAddress load error — the uniqueness check runs over the
        // post-inline address set.
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"oscillator","address":"/sub/osc"},
            {"type":"subpatch","address":"/sub","patch":"v"}]}"#;
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver(VOICE_IFACE)),
            Err(LoadError::DuplicateAddress(a)) if a == "/sub/osc"
        ));
    }

    #[test]
    fn nested_in_nested_compounds_prefixes_and_reexports() {
        // Two levels: the middle patch nests a child and re-exports its boundary input as
        // its own (`freq: "/inner.freq"` resolves through the inner face). Addresses compound:
        // /outer/inner/osc. The outer literal flows through both boundaries to the innermost
        // oscillator.
        struct Chain;
        impl ResourceResolver for Chain {
            fn resolve(&self, s: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
                Err(crate::resources::ResolveError::NotFound(s.to_string()))
            }
            fn resolve_text(&self, source: &str) -> Result<String, crate::resources::ResolveError> {
                Ok(match source {
                    "mid.json" => {
                        r#"{"instrument":"mid",
                            "resources":{"leaf":"leaf.json"},
                            "interface":{"inputs":{"freq":"/inner.freq"},
                                         "outputs":{"audio":"/inner.audio"}},
                            "nodes":[{"type":"subpatch","address":"/inner","patch":"leaf"}]}"#
                    }
                    _ => {
                        r#"{"instrument":"leaf",
                            "interface":{"inputs":{"freq":"/osc.freq"},
                                         "outputs":{"audio":"/osc.audio"}},
                            "nodes":[{"type":"oscillator","address":"/osc"}],
                            "outputs":[{"node":"/osc","port":"audio"}]}"#
                    }
                }
                .to_string())
            }
        }
        let json = r#"{"instrument":"p","resources":{"m":"mid.json"},"nodes":[
            {"type":"subpatch","address":"/outer","patch":"m","inputs":{"freq":330.0}},
            {"type":"output","address":"/out","inputs":{"audio":{"from":"/outer.audio"}}}],
            "outputs":[{"node":"/out","port":"audio"}]}"#;
        let loaded = load_instrument(json, &reg(), &Chain).expect("load");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let g = &loaded.graph;
        assert!(g.find("/outer").is_none(), "outer subpatch dissolved");
        assert!(g.find("/outer/inner").is_none(), "inner subpatch dissolved");
        let osc = g
            .find("/outer/inner/osc")
            .expect("compounded prefix reaches the innermost node");
        // The outer boundary literal seeds mid's re-export pipe; the value then flows through
        // the pipe chain to the innermost oscillator at render.
        let outer_pipe = g.find("/outer/freq").expect("mid's migrated freq pipe");
        assert!(g.nodes[outer_pipe]
            .value_overrides
            .iter()
            .any(|(_, a)| *a == Arg::F32(330.0)));
        let inner_pipe = g.find("/outer/inner/freq").expect("leaf's freq pipe");
        let osc_freq = g.nodes[osc]
            .descriptor
            .inputs
            .iter()
            .position(|p| p.name == "freq")
            .unwrap();
        // The chain: outer pipe → inner pipe → oscillator, plus the boundary wire onto /out.
        assert!(g
            .connections
            .iter()
            .any(|c| c.src == outer_pipe && c.dst == inner_pipe));
        assert!(g
            .connections
            .iter()
            .any(|c| c.src == inner_pipe && c.dst == osc && c.dst_port == osc_freq));
        let out = g.find("/out").unwrap();
        assert!(g.connections.iter().any(|c| c.src == osc && c.dst == out));
        assert_eq!(g.connections.len(), 3);
    }

    #[test]
    fn an_internally_driven_v1_boundary_input_is_dropped_by_migration() {
        // v1 could expose an input whose inner Signal port the child drove internally — a name
        // a host could see but never wire (the old fatal `BoundaryInputDriven`). The flip
        // cannot express that state: migration drops the entry **loudly** — a
        // `Migration` warning names it — and leaves the name dark, so the parent's wire onto
        // it degrades (dropped, like any dark boundary reference) instead of turning a
        // loadable v1 pair fatal.
        const DRIVEN_CHILD: &str = r#"{
            "instrument": "fx",
            "interface": { "inputs": { "audio": "/out.audio" } },
            "nodes": [
                { "type": "oscillator", "address": "/osc" },
                { "type": "output", "address": "/out",
                  "inputs": { "audio": { "from": "/osc.audio" } } }
            ]
        }"#;
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"oscillator","address":"/mod"},
            {"type":"subpatch","address":"/sub","patch":"v",
             "inputs":{"audio":{"from":"/mod.audio"}}}]}"#;
        let loaded =
            load_instrument(json, &reg(), &PatchResolver(DRIVEN_CHILD)).expect("load degrades");
        assert!(
            loaded.warnings.iter().any(|w| matches!(
                unwrap_nested(w),
                LoadWarning::Migration { name, detail }
                    if name == "audio" && detail.contains("internal wire")
            )),
            "the dropped entry is warned, nested under the referencing node: {:?}",
            loaded.warnings
        );
        // The same boundary input left unwired inside the child migrates to a pipe and accepts
        // the parent's wire: parent → pipe.in, pipe.out → inner target (two ordinary edges).
        const OPEN_CHILD: &str = r#"{
            "instrument": "fx",
            "interface": { "inputs": { "audio": "/out.audio" } },
            "nodes": [ { "type": "output", "address": "/out" } ]
        }"#;
        let loaded = load_instrument(json, &reg(), &PatchResolver(OPEN_CHILD)).expect("load");
        assert_eq!(loaded.graph.connections.len(), 2);
    }

    #[test]
    fn missing_grandchild_degrades_dark_transitively() {
        // The review-repro shape: mid re-exports its whole interface from a leaf whose file is
        // missing. Mid loads with a warning and records `freq`/`audio` as **dark** interface
        // entries; the parent's boundary literal, wire, and tap onto them then degrade exactly
        // like references to a dark nest — dropped with the warning — instead of escalating to
        // a fatal UnknownInput/UnknownPort one level up (dark is transitive).
        struct MissingLeaf;
        impl ResourceResolver for MissingLeaf {
            fn resolve(&self, s: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
                Err(crate::resources::ResolveError::NotFound(s.to_string()))
            }
            fn resolve_text(&self, source: &str) -> Result<String, crate::resources::ResolveError> {
                match source {
                    "mid.json" => Ok(r#"{"instrument":"mid",
                        "resources":{"leaf":"leaf.json"},
                        "interface":{"inputs":{"freq":"/inner.freq"},
                                     "outputs":{"audio":"/inner.audio"}},
                        "nodes":[{"type":"subpatch","address":"/inner","patch":"leaf"}]}"#
                        .to_string()),
                    other => Err(crate::resources::ResolveError::NotFound(other.to_string())),
                }
            }
        }
        let json = r#"{"instrument":"p","resources":{"m":"mid.json"},"nodes":[
            {"type":"subpatch","address":"/outer","patch":"m","inputs":{"freq":220.0}},
            {"type":"output","address":"/out","inputs":{"audio":{"from":"/outer.audio"}}}],
            "outputs":[{"node":"/outer","port":"audio"},{"node":"/out","port":"audio"}]}"#;
        let loaded = load_instrument(json, &reg(), &MissingLeaf)
            .expect("a missing grandchild must stay non-fatal at every level");
        let g = &loaded.graph;
        assert!(g.find("/out").is_some());
        assert!(
            g.connections.is_empty(),
            "the wire from the dark boundary port is dropped"
        );
        assert_eq!(g.outputs.len(), 1, "dark tap dropped; /out's tap survives");
        // The leaf's unavailability warned, and each dropped interface entry warned too.
        assert!(
            loaded
                .warnings
                .iter()
                .any(|w| matches!(unwrap_nested(w), LoadWarning::ResolveFailed { .. })),
            "{:?}",
            loaded.warnings
        );
        // Mid's `audio` output pipe feeds from the dark inner face: dropped + recorded dark.
        // (Its `freq` INPUT migrated to a fallback pipe instead — an input pipe is
        // self-contained in v2, so it never goes dark; the parent's literal landed on it.)
        assert!(
            loaded.warnings.iter().any(|w| matches!(unwrap_nested(w),
                    LoadWarning::DarkInterfaceEntry { name, .. } if name == "audio")),
            "{:?}",
            loaded.warnings
        );
        // A name the child never declared stays fatal — darkness never swallows a typo.
        let typo = r#"{"instrument":"p","resources":{"m":"mid.json"},"nodes":[
            {"type":"subpatch","address":"/outer","patch":"m","inputs":{"nope":220.0}}]}"#;
        assert!(matches!(
            load_instrument(typo, &reg(), &MissingLeaf),
            Err(LoadError::UnknownInput { node, input }) if node == "/outer" && input == "nope"
        ));
    }

    /// Peel [`LoadWarning::Nested`] wrappers to the innermost warning.
    fn unwrap_nested(w: &LoadWarning) -> &LoadWarning {
        match w {
            LoadWarning::Nested { warning, .. } => unwrap_nested(warning),
            other => other,
        }
    }

    #[test]
    fn child_master_taps_do_not_cross_the_boundary() {
        // The interface is the whole contract: a child's own master `outputs` taps vanish on
        // inline — a nested patch feeds the parent only through its boundary outputs.
        const TAPPED_CHILD: &str = r#"{
            "instrument": "standalone",
            "interface": { "outputs": { "audio": "/osc.audio" } },
            "nodes": [ { "type": "oscillator", "address": "/osc" } ],
            "outputs": [ { "node": "/osc", "port": "audio" } ]
        }"#;
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v"}]}"#;
        let loaded = load_instrument(json, &reg(), &PatchResolver(TAPPED_CHILD)).expect("load");
        assert!(
            loaded.graph.outputs.is_empty(),
            "child taps must not reach the parent master"
        );
    }

    #[test]
    fn reused_subpatch_decodes_each_sample_source_once() {
        // Two reuses of a sample-bearing child still build two graphs (state isolation), but the
        // fetch + decode goes through the load-wide source cache: one resolve() per source, and
        // both stores share the Arc'd buffer.
        use std::cell::Cell;
        const SAMPLED_CHILD: &str = r#"{"instrument":"c",
            "resources":{"kick":"kick.wav"},
            "interface":{"outputs":{"audio":"/s.audio"}},
            "nodes":[{"type":"sample","address":"/s","sample":"kick"}],
            "outputs":[{"node":"/s","port":"audio"}]}"#;
        struct Counting(Cell<usize>);
        impl ResourceResolver for Counting {
            fn resolve(&self, _s: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
                self.0.set(self.0.get() + 1);
                Ok(SampleBuffer::new(vec![vec![0.5, -0.5]], 48_000.0))
            }
            fn resolve_text(&self, _s: &str) -> Result<String, crate::resources::ResolveError> {
                Ok(SAMPLED_CHILD.to_string())
            }
        }
        let json = r#"{"instrument":"p","resources":{"v":"c.json"},"nodes":[
            {"type":"subpatch","address":"/a","patch":"v"},
            {"type":"subpatch","address":"/b","patch":"v"}]}"#;
        let counting = Counting(Cell::new(0));
        let loaded = load_instrument(json, &reg(), &counting).expect("load");
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert!(loaded.graph.find("/a/s").is_some() && loaded.graph.find("/b/s").is_some());
        assert_eq!(
            counting.0.get(),
            1,
            "two reuses of one sample source must decode it once"
        );
    }

    #[test]
    fn config_on_a_subpatch_node_is_fatal() {
        // The subpatch descriptor declares no Constants and the schema locks its `config` to
        // additionalProperties: false — the loader agrees: a stray config entry is
        // UnknownConfig, not silently ignored.
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v","config":{"voices":4}}]}"#;
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver(VOICE_IFACE)),
            Err(LoadError::UnknownConfig { node, name }) if node == "/sub" && name == "voices"
        ));
    }

    #[test]
    fn subpatch_without_patch_key_warns_and_dissolves_dark() {
        // The author wrote a subpatch node but forgot the `patch` key entirely: an authoring
        // mistake, not an availability failure. Pre-inline (P3) this failed loud; dissolving
        // silently would turn the typo invisible — so it degrades dark like a missing child,
        // but with a NoPatchRef warning naming the node.
        let json = r#"{"instrument":"p","nodes":[
            {"type":"subpatch","address":"/sub"},
            {"type":"output","address":"/out","inputs":{"audio":{"from":"/sub.audio"}}}],
            "outputs":[{"node":"/out","port":"audio"}]}"#;
        let loaded = load_instrument(json, &reg(), &PatchResolver(VOICE_IFACE)).expect("non-fatal");
        assert!(loaded.graph.find("/sub").is_none());
        assert!(loaded.graph.connections.is_empty(), "dark wire dropped");
        assert!(matches!(
            loaded.warnings.as_slice(),
            [LoadWarning::NoPatchRef { node }] if node == "/sub"
        ));
    }

    #[test]
    fn subpatch_missing_reference_warns_and_dissolves_dark() {
        // A `patch` id absent from the `resources` table degrades to a warning: the
        // reference dissolves dark — no node, no children — and wires/taps touching it are
        // dropped, so the instrument still loads and plays (silence through the nest), like a
        // missing voice patch. Never a hard error.
        let json = r#"{"instrument":"p","nodes":[
            {"type":"subpatch","address":"/sub","patch":"absent"},
            {"type":"output","address":"/out","inputs":{"audio":{"from":"/sub.audio"}}}],
            "outputs":[{"node":"/sub","port":"audio"},{"node":"/out","port":"audio"}]}"#;
        let loaded = load_instrument(json, &reg(), &PatchResolver(VOICE_IFACE)).expect("non-fatal");
        assert!(
            loaded.graph.find("/sub").is_none(),
            "dark nest leaves no node"
        );
        assert!(
            loaded.graph.connections.is_empty(),
            "the wire from the dark boundary is dropped"
        );
        assert_eq!(
            loaded.graph.outputs.len(),
            1,
            "the dark tap is dropped; /out's own tap survives"
        );
        // The warning names the slot that actually failed — a `patch` ref, not a sample —
        // and the migrated output pipe feeding from the dark nest warned as it was dropped.
        assert!(matches!(
            loaded.warnings.as_slice(),
            [
                LoadWarning::MissingResource { slot: "patch", .. },
                LoadWarning::DarkInterfaceEntry { .. }
            ]
        ));
        assert_eq!(
            loaded.warnings[0].to_string(),
            r#"node "/sub": patch "absent" not in resources table"#
        );
    }

    #[test]
    fn subpatch_resolve_failure_warns_and_dissolves_dark() {
        // The id resolves to a source, but `resolve_text` fails (the default `Failing` resolver):
        // non-fatal — a ResolveFailed warning, never a hard error. Like the
        // missing-id case, the reference dissolves dark: nothing is spliced in.
        struct Failing;
        impl ResourceResolver for Failing {
            fn resolve(&self, s: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
                Err(crate::resources::ResolveError::NotFound(s.to_string()))
            }
        }
        let json = r#"{"instrument":"p","resources":{"v":"missing.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v"}]}"#;
        let loaded = load_instrument(json, &reg(), &Failing).expect("non-fatal");
        // The failure is wrapped with the referencing node (ResolveFailed carries no node itself).
        assert!(matches!(
            loaded.warnings.as_slice(),
            [LoadWarning::Nested { node, warning }]
                if node == "/sub"
                    && matches!(warning.as_ref(), LoadWarning::ResolveFailed { slot: "patch", .. })
        ));
        assert!(loaded.graph.nodes.is_empty(), "nothing spliced in");
    }

    #[test]
    fn child_warnings_carry_the_subpatch_provenance() {
        // A warning from inside the child (its own `patch` id missing from its resources table)
        // surfaces on the parent wrapped in `Nested`, so the child-relative address is not mistaken
        // for a parent node and two same-shaped children stay distinguishable.
        const CHILD: &str = r#"{"instrument":"c","nodes":[
            {"type":"subpatch","address":"/inner","patch":"absent"}]}"#;
        let json = r#"{"instrument":"p","resources":{"c":"c.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"c"}]}"#;
        let loaded = load_instrument(json, &reg(), &PatchResolver(CHILD)).expect("load");
        assert_eq!(loaded.warnings.len(), 1);
        assert_eq!(
            loaded.warnings[0].to_string(),
            r#"in "/sub": node "/inner": patch "absent" not in resources table"#
        );
    }

    #[test]
    fn diamond_reuse_reads_the_source_once() {
        // Two subpatch nodes sharing one id fetch + parse the source once (the pass's per-id
        // dedup, like the sample pass's `handles`); each node still gets its own built child.
        use std::cell::Cell;
        struct Counting(Cell<usize>);
        impl ResourceResolver for Counting {
            fn resolve(&self, s: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
                Err(crate::resources::ResolveError::NotFound(s.to_string()))
            }
            fn resolve_text(&self, _: &str) -> Result<String, crate::resources::ResolveError> {
                self.0.set(self.0.get() + 1);
                Ok(VOICE_IFACE.to_string())
            }
        }
        let json = r#"{"instrument":"p","resources":{"v":"voices/lead.json"},"nodes":[
            {"type":"subpatch","address":"/one","patch":"v"},
            {"type":"subpatch","address":"/two","patch":"v"}]}"#;
        let resolver = Counting(Cell::new(0));
        let loaded = load_instrument(json, &reg(), &resolver).expect("load");
        assert_eq!(resolver.0.get(), 1, "one read serves both nodes");
        // Each reuse still gets its own inlined child — disjoint prefixes, disjoint state.
        assert!(loaded.graph.find("/one/osc").is_some());
        assert!(loaded.graph.find("/two/osc").is_some());
    }

    #[test]
    fn subpatch_structural_error_is_fatal() {
        // The sub-patch resolves but is structurally broken (unknown operator type): fatal, matching
        // the voice path — availability warns, structure/wiring errors.
        const BROKEN: &str = r#"{"instrument":"v","nodes":[{"type":"nope","address":"/x"}]}"#;
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v"}]}"#;
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver(BROKEN)),
            Err(LoadError::UnknownType { .. })
        ));
    }

    #[test]
    fn subpatch_malformed_json_is_fatal() {
        // The split lands at the fetch seam: once the text is in hand,
        // JSON that fails to parse is a structural error in the referenced patch — fatal, like an
        // unknown operator type — not an availability warning.
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v"}]}"#;
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver("{not json")),
            Err(LoadError::Json(_))
        ));
    }

    #[test]
    fn subpatch_self_reference_is_a_cycle_error() {
        // A patch whose `patch` resource resolves back to itself must fail as a structural
        // CyclicResource error, not recurse until the stack overflows. `PatchResolver` returns the
        // same text for every source, so the parent's own document is the "child" — the second
        // resolve of "self.json" re-enters a source still on the loading stack.
        let json = r#"{"instrument":"a","resources":{"me":"self.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"me"}]}"#;
        // Leak the parent text so the resolver can hand it back as the child (test-only).
        let resolver = PatchResolver(String::leak(json.to_string()));
        assert!(matches!(
            load_instrument(json, &reg(), &resolver),
            Err(LoadError::CyclicResource { source }) if source == "self.json"
        ));
    }

    #[test]
    fn subpatch_mutual_cycle_is_a_cycle_error() {
        // A -> B -> A through two `subpatch` nodes: the guard catches the re-entry on "a.json"
        // wherever the load started. Also proves the guard is a *stack*, not a per-call flag —
        // the chain crosses two distinct sources before looping.
        struct TwoDocs;
        impl ResourceResolver for TwoDocs {
            fn resolve(&self, s: &str) -> Result<SampleBuffer, crate::resources::ResolveError> {
                Err(crate::resources::ResolveError::NotFound(s.to_string()))
            }
            fn resolve_text(&self, source: &str) -> Result<String, crate::resources::ResolveError> {
                Ok(match source {
                    "a.json" => {
                        r#"{"instrument":"a","resources":{"b":"b.json"},"nodes":[
                        {"type":"subpatch","address":"/sub","patch":"b"}]}"#
                    }
                    _ => {
                        r#"{"instrument":"b","resources":{"a":"a.json"},"nodes":[
                        {"type":"subpatch","address":"/sub","patch":"a"}]}"#
                    }
                }
                .to_string())
            }
        }
        assert!(matches!(
            resolve_instrument("a.json", &reg(), &TwoDocs),
            Err(LoadError::CyclicResource { source }) if source == "a.json"
        ));
    }

    #[test]
    fn voice_self_reference_is_a_cycle_error() {
        // The voicer's `voice` slot rides the same recursive load, so the same guard covers a
        // voice patch that references itself (voice -> voice, or any voice/subpatch mix).
        let json = r#"{"instrument":"v","resources":{"me":"self.json"},"nodes":[
            {"type":"voicer","address":"/voicer","voice":"me"}]}"#;
        let resolver = PatchResolver(String::leak(json.to_string()));
        assert!(matches!(
            load_instrument(json, &reg(), &resolver),
            Err(LoadError::CyclicResource { source }) if source == "self.json"
        ));
    }

    #[test]
    fn diamond_reuse_is_not_a_cycle() {
        // Two sibling subpatch nodes referencing the same child is reuse, not a cycle: the guard
        // pops each source after its load completes, so only re-entry *while still loading* errors.
        let json = r#"{"instrument":"p","resources":{"v":"voices/lead.json"},"nodes":[
            {"type":"subpatch","address":"/one","patch":"v"},
            {"type":"subpatch","address":"/two","patch":"v"}]}"#;
        let loaded =
            load_instrument(json, &reg(), &PatchResolver(VOICE_IFACE)).expect("reuse loads");
        assert!(loaded.warnings.is_empty());
        assert!(loaded.graph.find("/one/osc").is_some());
        assert!(loaded.graph.find("/two/osc").is_some());
    }

    #[test]
    fn patch_ref_on_non_subpatch_errors() {
        // A `patch` ref is only valid on an operator declaring the slot (subpatch). On an oscillator
        // it is a structural misuse — the same generic resource-slot check that guards sample/voice.
        let json = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"oscillator","address":"/osc","patch":"v"}]}"#;
        assert!(matches!(
            load(json, &reg()),
            Err(LoadError::UnknownResource { .. })
        ));
    }

    #[test]
    fn patch_ref_round_trips_through_the_document() {
        // The nested reference lives in the *document*: parse → re-serialize preserves `patch`
        // via serde. (A built graph holds only the flattened equivalent — see the test below;
        // reference-preserving save from a built graph is the library thread, P7/#122.)
        let doc = NormalizedDoc::from_json(PARENT_WITH_SUBPATCH, &reg(), None).expect("parse");
        let reparsed =
            NormalizedDoc::from_json(&doc.to_json_pretty(), &reg(), None).expect("reparse");
        assert_eq!(doc, reparsed);
        assert_eq!(reparsed.nodes[0].patch.as_deref(), Some("myvoice"));
    }

    #[test]
    fn from_graph_saves_the_flattened_equivalent() {
        // The subpatch dissolves at build, so saving a built graph emits the inlined
        // child nodes under their prefixed addresses — no `subpatch` node, no `patch` ref. The
        // deliberate P4 shape; reference-preserving save is P7 (#122).
        let loaded = load_instrument(PARENT_WITH_SUBPATCH, &reg(), &PatchResolver(VOICE_IFACE))
            .expect("load");
        let saved = NormalizedDoc::from_graph(&loaded.graph, "p", &reg());
        let addrs: Vec<&str> = saved.nodes.iter().map(|n| n.address.as_str()).collect();
        assert_eq!(addrs, ["/sub/env", "/sub/osc"], "flattened, prefixed nodes");
        assert!(saved.nodes.iter().all(|n| n.patch.is_none()));
        assert!(saved.nodes.iter().all(|n| n.type_name != "subpatch"));
    }

    #[test]
    fn from_graph_routes_voices_to_config() {
        let json = r#"{"instrument":"t","nodes":[
            {"type":"voicer","address":"/v","config":{"voices":5}}]}"#;
        let g = load(json, &reg()).expect("load");
        let saved = NormalizedDoc::from_graph(&g, "t", &reg());
        let v = saved.nodes.iter().find(|n| n.address == "/v").unwrap();
        assert!(matches!(
            v.config.get("voices"),
            Some(ConfigValue::Number(_))
        ));
        assert!(!v.inputs.contains_key("voices"));
    }
}
