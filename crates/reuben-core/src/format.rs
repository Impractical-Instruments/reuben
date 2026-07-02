//! Instrument format — the JSON canonical document (ADR-0004, ADR-0028).
//!
//! An instrument is plain data: a list of operator `nodes`, each carrying one `inputs` map
//! (ADR-0028) and an optional `config` block, plus master `outputs`. A node's `inputs` entry is
//! either a **literal** (a number, or an `Enum` symbol like `"Hp"`) or a **wire-ref** to another
//! node's output (`{ "from": "/osc.audio" }`, or `{ "from": "/osc" }` when the source has a single
//! output). `config` carries instantiate-time **`Constant`s** (e.g. a voicer's `voices`). Ports are
//! referenced by **name** (from the operator's [`Descriptor`](crate::descriptor::Descriptor)), not
//! by brittle index. Optional `doc` fields carry human/agent notes. The schema that validates these
//! documents is generated from the operator descriptors ([`crate::schema`]).
//!
//! [`load`] turns JSON into a [`Graph`] (resolving types via a [`Registry`]);
//! [`InstrumentDoc::from_graph`] goes the other way. Loading is an authoring step, not a realtime
//! path — it lives in the portable core but never runs on the audio thread.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::descriptor::{Descriptor, PortType};
use crate::graph::{Graph, Interface};
use crate::message::Arg;
use crate::plan::{port_kind, PortKind};
use crate::registry::Registry;
use crate::resources::{ResolvedRefs, ResourceResolver, ResourceStore, SampleBuffer, SampleId};

/// A complete instrument document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentDoc {
    /// Human-facing name / id of this instrument.
    pub instrument: String,
    /// Optional note for humans and agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Decoded-resource table (ADR-0016): logical id → source (a file path today). A node
    /// references a resource by id via its `sample` field; the loader resolves+decodes each
    /// referenced id once (dedup) into the [`ResourceStore`]. Entries no node uses are
    /// ignored.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub resources: BTreeMap<String, String>,
    /// Engine-honored I/O boundary (ADR-0032 §1): external names → internal `node.port` refs the
    /// engine binds and type-checks. A voice patch declares this so its host Voicer can drive its
    /// `freq`/`gate` and tap its `audio`/`active`. `None` (the common case) for a top-level rig.
    /// Distinct from a node's `control` (ADR-0018), which is opaque, engine-ignored UI metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<InterfaceDoc>,
    pub nodes: Vec<NodeDoc>,
    #[serde(default)]
    pub outputs: Vec<PortRef>,
}

/// A document's `interface` block (ADR-0032 §1): the named I/O boundary, as external name → internal
/// `node.port` wire-ref (the same `/node.port` form `inputs` wire-refs use; the sole-output sugar
/// `/node` is allowed for an `outputs` ref). `inputs` names map to internal **input** ports;
/// `outputs` names to internal **output** ports. Resolved + direction-checked at
/// [`build`](InstrumentDoc::build) into [`Interface`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceDoc {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InterfaceEntry>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, InterfaceEntry>,
}

/// One `interface` entry: the internal target, optionally decorated with presentational
/// overrides (ADR-0034 §4). A JSON string is the bare target (`"wet": "/mix.wet"`); a JSON
/// object is the [`Detailed`](Self::Detailed) form carrying per-field metadata overrides.
/// Deserialization dispatches on the JSON type by hand (not `#[serde(untagged)]`) so a
/// malformed object keeps [`InterfaceMeta`]'s pointed field-level errors — "unknown field
/// `lable`", "missing field `target`" — instead of collapsing into one opaque no-variant error.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum InterfaceEntry {
    /// The common bare form: just the internal `/node.port` wire-ref.
    Target(String),
    /// Target plus presentational-metadata overrides (label, unit, range, widget).
    Detailed(InterfaceMeta),
}

/// The object form of an [`InterfaceEntry`] (ADR-0034 §4): the internal target plus
/// **presentational** metadata overriding what the boundary port inherits from the inner port —
/// how a control *presents* (label, unit, range, widget), consumed by introspection (`describe`)
/// and control-surface generation (ADR-0017/0018). The `Arg` **type is inherited and not
/// overridable** — there is deliberately no field to express one, and `deny_unknown_fields`
/// rejects an attempt, so the boundary can never lie to the type-checker (§4/§5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InterfaceMeta {
    /// The internal `/node.port` wire-ref this external name resolves to.
    pub target: String,
    /// Display name override (the boundary name itself is the wiring handle; this is UI-facing).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Unit override (e.g. `"Hz"`, `"%"`), replacing the inner port's unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    /// Widget hint override for a generated control surface (ADR-0018), e.g. `"knob"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget: Option<String>,
    /// Range-minimum override (presentational: narrows/renames the swept range a control shows).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    /// Range-maximum override (see `min`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

impl InterfaceEntry {
    /// The internal `/node.port` wire-ref, whichever form carries it.
    pub fn target(&self) -> &str {
        match self {
            InterfaceEntry::Target(t) => t,
            InterfaceEntry::Detailed(m) => &m.target,
        }
    }

    /// The presentational overrides, when the entry carries any (the object form).
    pub fn meta(&self) -> Option<&InterfaceMeta> {
        match self {
            InterfaceEntry::Target(_) => None,
            InterfaceEntry::Detailed(m) => Some(m),
        }
    }
}

impl From<String> for InterfaceEntry {
    fn from(target: String) -> Self {
        InterfaceEntry::Target(target)
    }
}

impl<'de> Deserialize<'de> for InterfaceEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct EntryVisitor;
        impl<'de> serde::de::Visitor<'de> for EntryVisitor {
            type Value = InterfaceEntry;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a \"/node.port\" target string or an override object with `target`")
            }

            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(InterfaceEntry::Target(v.to_string()))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                InterfaceMeta::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(InterfaceEntry::Detailed)
            }
        }
        deserializer.deserialize_any(EntryVisitor)
    }
}

/// One operator instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDoc {
    /// Operator type name (must be registered, e.g. `"oscillator"`).
    #[serde(rename = "type")]
    pub type_name: String,
    /// OSC address / routing prefix, e.g. `"/osc"`. Unique within the instrument.
    pub address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doc: Option<String>,
    /// Instantiate-time **`Constant`s** (ADR-0028) by name, e.g. `{ "voices": 8 }`. A name here
    /// must be a declared [`Constant`](Descriptor::constants); a runtime input set here, or a
    /// constant set in `inputs`, is a load error.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub config: BTreeMap<String, ConfigValue>,
    /// One value per wired/settable input (ADR-0028): a **literal** (a number, or an `Enum` symbol
    /// like `"Hp"`) or a **wire-ref** (`{ "from": "/node.port" }`). Replaces the old `params` map
    /// and top-level `connections` array — a `Float` input and the wire that drives it now target
    /// the same slot. Omitted inputs use the descriptor default.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub inputs: BTreeMap<String, InputValue>,
    /// Resource reference (ADR-0016): a logical id into the document's `resources` table.
    /// Only valid on an operator whose descriptor declares a `sample` resource slot (the
    /// sample player); rejected elsewhere as a structural [`LoadError::UnknownResource`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample: Option<String>,
    /// Instrument-resource reference (ADR-0032 §2): a logical id into the document's `resources`
    /// table whose source is a **voice patch** (a standalone instrument JSON). Only valid on an
    /// operator declaring a `voice` resource slot (the Voicer); rejected elsewhere as a structural
    /// [`LoadError::UnknownResource`]. The loader builds the patch `voices` times and binds the
    /// graphs via [`Operator::bind_voices`](crate::operator::Operator::bind_voices).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// Nested-instrument reference (ADR-0034 §1): a logical id into the document's `resources` table
    /// whose source is another instrument patch. Only valid on the built-in `subpatch` operator
    /// (which declares a `patch` resource slot); rejected elsewhere as a structural
    /// [`LoadError::UnknownResource`]. At build the referenced patch is loaded recursively and
    /// **inlined** (ADR-0034 §2, nesting P4): its nodes are spliced into this graph under this
    /// node's address prefix, the boundary face is synthesized from its `interface`, and the
    /// `subpatch` node dissolves — it never reaches the built [`Graph`]. The reference survives in
    /// the *document* only; `from_graph` of a built graph emits the flattened equivalent (P7).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patch: Option<String>,
    /// Public-control metadata for a generated control surface (ADR-0018): marks this node as
    /// player-facing and carries display hints (`label`, optional `unit`/`widget`/range). The
    /// engine never reads it — it is passed through opaquely so it survives load → round-trip →
    /// re-serialize (serde would otherwise drop an unknown field, erasing it on `from_graph`).
    /// A control-surface generator reads it; `None` means the node is internal plumbing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<serde_json::Value>,
}

impl NodeDoc {
    /// This node's resource references paired with the slot each targets (ADR-0016/0032/0034): the
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

/// One [`NodeDoc::inputs`] value (ADR-0028): a wire-ref, an `Enum` symbol, or a numeric literal.
///
/// Untagged: a JSON object `{ "from": ... }` is a [`Wire`](Self::Wire); a JSON string is a
/// [`Symbol`](Self::Symbol) (an `Enum` variant name); a JSON number is a [`Number`](Self::Number)
/// (a `Float`/param value, or an `Enum` index fallback).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InputValue {
    /// A wire-ref to a source output: `"/node.port"`, or `"/node"` when the source has exactly
    /// one output (the sole-output sugar).
    Wire { from: String },
    /// An `Enum` input symbol, e.g. `"Hp"` (ADR-0028 enum-over-the-wire, symbol primary).
    Symbol(String),
    /// A numeric literal — a `Float` input/param value, or an `Enum` variant index (fallback form).
    Number(f64),
}

/// One [`NodeDoc::config`] value (ADR-0028): an instantiate-time `Constant`.
///
/// Untagged: a JSON number is a [`Number`](Self::Number) (an `Int` constant such as `voices`); a
/// JSON string is a [`Symbol`](Self::Symbol) (an `Enum` constant, none today). Floats are accepted
/// and rounded so `8` and `8.0` both name 8 voices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConfigValue {
    /// An `Int` constant (e.g. `voices`), applied rounded.
    Number(f64),
    /// An `Enum` constant symbol (none today; forward-compatible).
    Symbol(String),
}

/// A reference to one node's port, by names. Used only in `outputs` (a master tap, ADR-0026);
/// node-to-node wiring lives in [`NodeDoc::inputs`] as a [`InputValue::Wire`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortRef {
    pub node: String,
    pub port: String,
    /// Logical master channel this tap feeds (ADR-0026): `0` = first channel (left), `1` = second
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
    /// a non-enum input, or a symbol/index that names no variant (ADR-0028: never snaps to default).
    BadInputValue {
        node: String,
        input: String,
        value: String,
    },
    /// A `config` name is not a declared [`Constant`](Descriptor::constants).
    UnknownConfig { node: String, name: String },
    /// A `Constant` (e.g. `voices`) appears in `inputs` — it must live in `config`, since changing
    /// it would rebuild the graph (ADR-0028).
    ConstantInInputs { node: String, name: String },
    /// A wire-ref uses the sole-output sugar (`"/node"`) but the source has more than one output,
    /// so the intended port is ambiguous.
    AmbiguousWire { node: String, reference: String },
    /// A wire joins two ports of incompatible [`PortType`]s (e.g. `Note` → `Buffer`) — the illegal
    /// wiring (ADR-0030). Equal types are fine, an `F32` source into a `Buffer` port is the one
    /// implicit ZOH bridge (ADR-0031 — the reverse, `Buffer` → `F32`, is Signal→Value and rejected:
    /// no implicit sample-and-hold), and an [`Arg`](PortType::Arg) pass-through input takes any
    /// source with an OSC form; everything else is rejected here. On a nested boundary wire,
    /// `from`/`to` name the **boundary** port (`/sub.audio`), never the prefixed internals
    /// (ADR-0034 §5).
    TypeMismatch {
        from: String,
        from_type: Box<PortType>,
        to: String,
        to_type: Box<PortType>,
    },
    /// A node carries a `sample` reference but its operator declares no such resource slot
    /// (ADR-0016) — a structural misuse, fatal like the other wiring errors.
    UnknownResource { node: String, slot: String },
    /// An instrument-resource `source` is referenced again while it is still being loaded — a
    /// `voice`/`patch` chain that (directly or transitively) contains itself (ADR-0032/0034).
    /// Loading it would recurse forever, so the cycle is a structural error, fatal like the
    /// other wiring errors.
    CyclicResource { source: String },
    /// An `interface` entry's presentational range override lies about what the engine enforces
    /// (ADR-0034 §4): a range override on a port with no numeric range, a bound outside the inner
    /// port's engine-clamped range, an inverted/empty advertised range, or an effective default
    /// outside the advertised range. `describe` publishes these values as the boundary contract
    /// and no engine path reconciles them, so advertised must stay a subset of enforced — checked
    /// here at load, named by the boundary port.
    InterfaceOverride { name: String, reason: String },
    /// A wire lands on a boundary input whose inner **Signal** port the nested child already
    /// drives with its own internal wire (ADR-0034). The plan reads exactly one inbound edge per
    /// Signal input, so the outer wire would load clean and do nothing — a silently dead wire,
    /// a state flat authoring can never produce (one `inputs` key, one wire). Fatal, named in
    /// boundary terms; a child that wants an externally wireable Signal port must leave it
    /// unwired internally. (Value/Event inputs merge message streams from several edges, so
    /// multiple drivers stay legal there.)
    BoundaryInputDriven { node: String, input: String },
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Json(e) => write!(f, "invalid JSON: {e}"),
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
                if matches!(**from_type, PortType::F32Buffer) && matches!(**to_type, PortType::F32)
                {
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
            LoadError::BoundaryInputDriven { node, input } => write!(
                f,
                "boundary input {node:?}.{input:?} is already driven by a wire inside the nested \
                 patch — an outside wire would be silently dead"
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
/// (nothing spliced in; wires touching it dropped — see [`InstrumentDoc::build`]). Use
/// [`load_instrument`] to resolve, bind, and inline.
pub fn load(json: &str, registry: &Registry) -> Result<Graph, LoadError> {
    InstrumentDoc::from_json(json)?.build(registry)
}

/// A non-fatal resource problem found at load (ADR-0016). The owning node still builds and
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
    /// A warning that arose while loading a **nested** instrument (a voice or subpatch child,
    /// ADR-0032/0034), contextualized by the referencing parent node so provenance survives the
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
    /// (ADR-0016/0034).
    DarkInterfaceEntry {
        /// The external interface name that vanished.
        name: String,
        /// The internal reference it pointed at (`"/inner.freq"`).
        target: String,
    },
    /// A `subpatch` node carries no `patch` reference at all (ADR-0034 §1) — an authoring
    /// mistake, not an availability failure, but the node still dissolves dark so the
    /// instrument stays playable (ADR-0016). Warned rather than silent: pre-inline this shape
    /// failed loud, and silence through the nest would hide the typo.
    NoPatchRef { node: String },
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
/// them (ADR-0016).
pub struct Loaded {
    pub graph: Graph,
    pub warnings: Vec<LoadWarning>,
}

/// Shared state threaded through the recursive nested-load passes (`voice`/`patch`,
/// ADR-0032/0034), one per top-level load.
#[derive(Default)]
struct LoadCtx {
    /// The cycle-guard stack: instrument-resource sources currently being resolved,
    /// root-first. A chain that re-enters a source still on the stack is the fatal
    /// [`LoadError::CyclicResource`] instead of infinite recursion.
    loading: Vec<String>,
    /// Decoded-sample cache, keyed by source (the same identity the `resources` table and the
    /// cycle guard use). Each subpatch reuse and voice copy builds its own graph and
    /// [`ResourceStore`], but a given source is fetched + decoded **once** per load; the
    /// stores share the `Arc`. Failures are deliberately not cached, so every referencing
    /// document still surfaces its own warning.
    samples: BTreeMap<String, Arc<SampleBuffer>>,
}

/// Parse, build, and **resolve + bind decoded resources** (ADR-0016) — the full authoring
/// load path. Structural/wiring problems are fatal ([`LoadError`]); resource problems are
/// non-fatal: a missing id or a resolve/decode failure binds the node to an empty buffer
/// (it plays silence) and is reported as a [`LoadWarning`]. Each referenced id is resolved
/// exactly once (dedup) via `resolver`; unreferenced `resources` entries are ignored.
pub fn load_instrument(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<Loaded, LoadError> {
    load_instrument_guarded(json, registry, resolver, &mut LoadCtx::default())
}

/// [`load_instrument`] from an already-parsed document — the parse-once entry point for a
/// caller that needs both the document (e.g. for its `interface` overrides,
/// [`crate::describe::describe_boundary`]) and the built graph, without re-parsing the JSON.
pub fn load_instrument_doc(
    doc: &InstrumentDoc,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<Loaded, LoadError> {
    load_doc_guarded(doc, registry, resolver, &mut LoadCtx::default())
}

/// [`load_instrument`] with the shared load state threaded through: `ctx` carries the cycle
/// guard (a chain that re-enters a source still loading is caught as
/// [`LoadError::CyclicResource`] instead of recursing forever) and the per-load decoded-sample
/// cache the recursive passes (`voice`, `patch`) share.
fn load_instrument_guarded(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    ctx: &mut LoadCtx,
) -> Result<Loaded, LoadError> {
    let doc = InstrumentDoc::from_json(json)?;
    load_doc_guarded(&doc, registry, resolver, ctx)
}

/// [`load_instrument_guarded`] from an already-parsed document — the subpatch pass parses a
/// shared child source once and loads it per referencing node through this.
fn load_doc_guarded(
    doc: &InstrumentDoc,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    ctx: &mut LoadCtx,
) -> Result<Loaded, LoadError> {
    // Build with the resolver threaded in (ADR-0034's resolution-ordering note): a `subpatch`
    // node's boundary face only exists once its child is resolved, and it must exist *during*
    // build pass 2 so the one wire type-checker covers boundary wires (§5) — so nested references
    // resolve and inline inside `build`, earlier than the ADR-0016 pipeline resolves `sample`/
    // `voice` refs below.
    let Loaded {
        mut graph,
        mut warnings,
    } = doc.build_nested(registry, Some(resolver), ctx)?;

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
            Some(source) => match ctx.samples.get(source) {
                Some(shared) => shared.clone(),
                None => match resolver.resolve(source) {
                    Ok(b) => {
                        let shared = Arc::new(b);
                        ctx.samples.insert(source.clone(), shared.clone());
                        shared
                    }
                    Err(e) => {
                        // Not cached: every referencing document keeps its own warning.
                        warnings.push(LoadWarning::ResolveFailed {
                            slot: "sample",
                            id: id.clone(),
                            source: source.clone(),
                            reason: e.to_string(),
                        });
                        Arc::new(SampleBuffer::empty())
                    }
                },
            },
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

    // Instrument-resource pass (ADR-0032 §2): a node with a `voice` ref hosts N copies of a voice
    // patch. Build the patch `voices` times (each an independent Graph — `Graph` is not `Clone`, and
    // `Plan::instantiate` consumes one Graph per voice) and bind them; the Voicer turns them into
    // per-voice sub-plans at `on_instantiate`. Structural errors in the patch are fatal; a missing id
    // or resolve failure degrades to silence (empty graphs) with a warning, like a sample.
    for n in &doc.nodes {
        let Some(id) = &n.voice else { continue };
        let Some(key) = graph.find(&n.address) else {
            continue;
        };
        let n_voices = voice_count(n, &graph.nodes[key].descriptor);
        let mut voices: Vec<Graph> = Vec::with_capacity(n_voices);
        match lookup_source(doc, &n.address, "voice", id, &mut warnings) {
            None => {
                for _ in 0..n_voices {
                    voices.push(Graph::new());
                }
            }
            Some(source) => {
                for i in 0..n_voices {
                    let loaded =
                        resolve_instrument_slotted(source, "voice", registry, resolver, ctx)?;
                    // One copy's warnings suffice — the N builds are identical. Wrapped with the
                    // hosting node so provenance survives the merge (see `LoadWarning::Nested`).
                    if i == 0 {
                        warnings
                            .extend(loaded.warnings.into_iter().map(|w| w.nested_in(&n.address)));
                    }
                    voices.push(loaded.graph);
                }
            }
        }
        graph.nodes[key].op.bind_voices(voices);
    }

    Ok(Loaded { graph, warnings })
}

/// Look up a node's resource `id` in the document's `resources` table — the shared first step of
/// every resource pass (`sample`/`voice`/`patch`). A miss is the non-fatal
/// [`LoadWarning::MissingResource`] (ADR-0016), pushed here so the policy lives in one place;
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

/// The voice-pool size for a Voicer node (ADR-0032): the node's value for the operator's
/// instantiate-time [`Constant`](Descriptor::constants) (the voicer's `voices` pool size),
/// else that Constant's descriptor default, floored to 1. Reads the generic Constant slot rather
/// than a hardcoded `"voices"` name, so the same machinery serves any future pool-sized operator.
/// An operator with no Constant has a pool of one.
fn voice_count(n: &NodeDoc, descriptor: &Descriptor) -> usize {
    let Some(constant) = descriptor.constants.first() else {
        return 1;
    };
    let default = match &constant.ty {
        PortType::I32 { meta: Some(m) } => m.default as f64,
        _ => 1.0,
    };
    let raw = n
        .config
        .get(constant.name)
        .and_then(|v| match v {
            ConfigValue::Number(x) => Some(*x),
            ConfigValue::Symbol(_) => None,
        })
        .unwrap_or(default);
    (raw.round() as i64).max(1) as usize
}

/// Resolve an **instrument-kind resource** (ADR-0032 §2): a patch `source` (a path) is read to its
/// JSON via [`ResourceResolver::resolve_text`], then built into a sub-[`Graph`] through the full
/// [`load_instrument`] path — so the sub-patch's own `sample` resources resolve recursively and its
/// `interface` boundary is resolved for the host to bind. Structural/wiring problems in the patch
/// are fatal ([`LoadError`]) — including JSON that fails to parse: the ADR-0016 split lands at the
/// fetch seam (ADR-0034 §1), so only *availability* degrades. A `resolve_text` failure is
/// **non-fatal**: it yields an empty graph plus a [`LoadWarning::ResolveFailed`], so one missing
/// voice patch never crashes the host. A `source` that (directly or transitively) references itself is fatal
/// ([`LoadError::CyclicResource`]) — the cycle guard that keeps recursive nesting (ADR-0034)
/// from overflowing the stack. This is the net-new piece ADR-0032 needs — "a resource that is a
/// Graph, not bytes."
pub fn resolve_instrument(
    source: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<Loaded, LoadError> {
    resolve_instrument_slotted(source, "patch", registry, resolver, &mut LoadCtx::default())
}

/// [`resolve_instrument`] with the cycle guard and the resource `slot` the ref came through (so
/// warnings name what actually failed), folding an unavailable source into the ADR-0032
/// degradation the voice pass wants: an empty graph carrying the warning (silence).
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
/// unavailable source (a `resolve_text` failure, non-fatal per ADR-0016) surfaced as the warning
/// so each caller picks its own degradation — the voice pass binds silence (empty graphs), the
/// subpatch pass dissolves the reference dark (nothing spliced in). `slot`
/// names the resource slot the ref came through (`"voice"`/`"patch"`) for the warning. Cycles
/// are refused before resolving: a `source` already on the `loading` stack (a voice/patch chain
/// re-entering itself) is a fatal [`LoadError::CyclicResource`], keyed on the source string — the
/// same identity the `resources` table resolves by.
fn try_resolve_instrument(
    source: &str,
    slot: &'static str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    ctx: &mut LoadCtx,
) -> Result<Result<Loaded, LoadWarning>, LoadError> {
    match resolver.resolve_text(source) {
        Ok(text) => {
            let doc = InstrumentDoc::from_json(&text)?;
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
/// [`LoadError::CyclicResource`] — keyed on the source string, the same identity the `resources`
/// table resolves by — otherwise push it and recurse through [`load_doc_guarded`].
fn load_child_guarded(
    doc: &InstrumentDoc,
    source: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
    ctx: &mut LoadCtx,
) -> Result<Loaded, LoadError> {
    if ctx.loading.iter().any(|s| s == source) {
        return Err(LoadError::CyclicResource {
            source: source.to_string(),
        });
    }
    ctx.loading.push(source.to_string());
    let result = load_doc_guarded(doc, registry, resolver, ctx);
    ctx.loading.pop();
    result
}

impl InstrumentDoc {
    /// Parse a document from JSON (no operator resolution yet).
    pub fn from_json(json: &str) -> Result<Self, LoadError> {
        serde_json::from_str(json).map_err(LoadError::Json)
    }

    /// Serialize to pretty JSON (the canonical on-disk form).
    pub fn to_json_pretty(&self) -> String {
        serde_json::to_string_pretty(self).expect("InstrumentDoc serializes")
    }

    /// Build the [`Graph`] this document describes.
    ///
    /// Two passes: pass 1 creates every node and applies its `config` constants and literal
    /// `inputs`; pass 2 resolves wire-refs (which may name a node declared later) into edges,
    /// type-checking each `Arg` type (ADR-0030). Between them, the subpatch pass (ADR-0034)
    /// inlines nested instruments.
    ///
    /// This path resolves no resources, so a nested reference cannot be loaded: every `subpatch`
    /// node dissolves *dark* — no child is spliced in, and wires/taps touching it are dropped
    /// (the same degradation an unavailable child gets on the full path). Use [`load_instrument`]
    /// to resolve and inline nested instruments.
    pub fn build(&self, registry: &Registry) -> Result<Graph, LoadError> {
        Ok(self
            .build_nested(registry, None, &mut LoadCtx::default())?
            .graph)
    }

    /// [`build`](Self::build) with the nesting machinery threaded through (ADR-0034's
    /// resolution-ordering note): `resolver` loads `subpatch` children so their boundary faces
    /// exist during pass 2 (`None` — the plain [`load`]/[`build`] path — dissolves every nested
    /// reference dark without loading it); `ctx` carries the cycle-guard stack and the decoded-
    /// sample cache shared with [`load_child_guarded`]. Returns the built graph plus
    /// availability warnings from nested loads.
    fn build_nested(
        &self,
        registry: &Registry,
        resolver: Option<&dyn ResourceResolver>,
        ctx: &mut LoadCtx,
    ) -> Result<Loaded, LoadError> {
        let mut graph = Graph::new();
        let mut warnings = Vec::new();
        // address -> (key, descriptor) for resolving wire-refs and outputs. Document nodes only:
        // spliced subpatch internals are deliberately not wireable — the boundary face is the
        // contract (ADR-0034 §3's namespace scopes OSC reachability, not wiring).
        let mut by_addr: BTreeMap<String, (crate::graph::NodeKey, Descriptor)> = BTreeMap::new();
        // Every claimed address — document nodes *and* spliced subpatch internals — so the
        // duplicate check also catches post-prefix collisions (fatal, ADR-0034 §3).
        let mut addresses: BTreeSet<String> = BTreeSet::new();
        // Synthesized boundary face per subpatch address (ADR-0034 §4): the owned-string port set
        // pass 2 resolves boundary wires against.
        let mut faces: BTreeMap<String, BoundaryFace> = BTreeMap::new();
        // Subpatch addresses that dissolved dark — child unavailable (a warning, ADR-0016) or no
        // resolver on this path. Wires and taps touching them are dropped, so the instrument
        // still loads and the nest plays as silence, like a missing voice patch.
        let mut dark: BTreeSet<String> = BTreeSet::new();

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
            // A resource ref is only valid on an operator that declares that slot (ADR-0016: a
            // `sample` on the sample player, a `voice` on the Voicer per ADR-0032 §2). Validate
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
                // A nested-instrument reference (ADR-0034): no graph node is created — the
                // subpatch pass below splices the child's nodes in and the reference dissolves
                // (§2). Its `inputs` are boundary values, validated there against the
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

            // `config`: every name must be a declared Constant (ADR-0035); apply it at its slot.
            for (name, value) in &n.config {
                if !descriptor.is_constant(name) {
                    return Err(LoadError::UnknownConfig {
                        node: n.address.clone(),
                        name: name.clone(),
                    });
                }
                match value {
                    ConfigValue::Number(v) => graph.set_constant(key, name, &Arg::F32(*v as f32)),
                    ConfigValue::Symbol(s) => graph.set_constant(key, name, &Arg::Str(s.clone())),
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

        // Subpatch pass (ADR-0034 §2, nesting P4): resolve each nested reference, load the child
        // through the full path (its own resources bind and its own nests inline — recursion),
        // then splice it into this graph: internal addresses take this node's address as a prefix,
        // edges are remapped, and the boundary face is synthesized from the child's `interface`.
        // The `subpatch` node itself never materializes — it dissolves into its child's nodes.
        // Structural errors in a resolved child stay fatal; availability failures leave the
        // address dark (see above). The source read + parse is deduped per id (like the sample
        // pass's `handles`); each node still builds its own child — `Graph` is not `Clone` — so
        // two reuses get disjoint nodes, addresses, and state for free.
        let mut patch_docs: BTreeMap<String, Option<InstrumentDoc>> = BTreeMap::new();
        for n in &self.nodes {
            let nested = registry
                .get(&n.type_name)
                .is_some_and(|e| e.descriptor.has_resource("patch"));
            if !nested {
                continue;
            }
            let Some(id) = &n.patch else {
                // No `patch` key at all: an authoring mistake, not an availability failure —
                // but the node still dissolves dark (ADR-0016 keeps the instrument playable).
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
            if !patch_docs.contains_key(id) {
                let child_doc = match resolver.resolve_text(source) {
                    Ok(text) => Some(InstrumentDoc::from_json(&text)?),
                    Err(e) => {
                        let failed = LoadWarning::ResolveFailed {
                            slot: "patch",
                            id: id.clone(),
                            source: source.clone(),
                            reason: e.to_string(),
                        };
                        warnings.push(failed.nested_in(&n.address));
                        None
                    }
                };
                patch_docs.insert(id.clone(), child_doc);
            }
            let Some(child_doc) = &patch_docs[id] else {
                dark.insert(n.address.clone());
                continue;
            };
            let loaded = load_child_guarded(child_doc, source, registry, resolver, ctx)?;
            warnings.extend(loaded.warnings.into_iter().map(|w| w.nested_in(&n.address)));
            let face = splice_subpatch(&mut graph, loaded.graph, &n.address, &mut addresses)?;

            // Boundary literals (ADR-0034 §1: `"wet": 0.3`): validated against the face and the
            // inner port the interface names, then applied as that inner node's value-override.
            // Errors speak in boundary terms — the subpatch address and external port name —
            // never the prefixed internal address.
            for (name, value) in &n.inputs {
                if matches!(value, InputValue::Wire { .. }) {
                    continue; // pass 2
                }
                let Some(fp) = face.input(name) else {
                    if face.dark_inputs.contains(name) {
                        continue; // dark boundary port: the literal is dropped (ADR-0016)
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
            faces.insert(n.address.clone(), face);
        }

        // Pass 2: wire-refs -> edges (Arg-type-checked). A subpatch endpoint resolves through its
        // synthesized boundary face (ADR-0034 §4/§5) to the inner `(node, port)` its interface
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
                // A dark boundary port drops the wire (ADR-0016), like a dark nest.
                let Some((dst_key, dst_port, to_ty)) =
                    resolve_input(&faces, &by_addr, &n.address, name)?
                else {
                    continue;
                };
                // A face input may land on an inner Signal port the child already drives with
                // its own wire (or that another boundary alias just wired). The plan reads
                // exactly one inbound edge per Signal input, so this wire would be silently
                // dead — fatal instead (`BoundaryInputDriven`). Only reachable through a face:
                // a document node takes one wire per input key, and spliced internals are not
                // wireable. Value/Event ports merge message edges, so several drivers are legal.
                if faces.contains_key(&n.address)
                    && port_kind(&graph.nodes[dst_key].descriptor.inputs[dst_port])
                        == PortKind::Signal
                    && graph
                        .connections
                        .iter()
                        .any(|c| c.dst == dst_key && c.dst_port == dst_port)
                {
                    return Err(LoadError::BoundaryInputDriven {
                        node: n.address.clone(),
                        input: name.clone(),
                    });
                }
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
                // implicit bridge** — Value→Signal, ZOH-materialized at the sink (ADR-0031). The
                // reverse, a `Buffer` source into an `F32` control port, is Signal→Value: a hard
                // error with no implicit sample-and-hold (ADR-0031 — an explicit sig→val converter
                // op is the sanctioned path). It is rejected *here*, not left to the plan's form
                // check, so a mistyped wire into a nested boundary fails at load named in boundary
                // terms (`/sub.audio`) instead of surfacing at instantiate as a FormMismatch on
                // the prefixed internals (ADR-0034 §5). A type-agnostic `Arg` pass-through input
                // (issue #141) is **capability-keyed**: it accepts any source whose type has an
                // external OSC form (`boundary::has_osc_form`, the single statement shared with
                // the plan check) — the primitives, a vocab enum, `Note`'s flat form. A `Buffer`
                // never emits Messages (audio stays off the wire, ADR-0026/0030) and `Harmony`
                // has no OSC form (converters: issue #146) — a wire that could never send
                // anything is rejected here, not left silently dead. Anything else is illegal.
                let compatible = from_ty == to_ty
                    || matches!((&from_ty, &to_ty), (PortType::F32, PortType::F32Buffer))
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

        // `outputs`: master taps (ADR-0026). A tap on a subpatch resolves through its face to the
        // inner output the interface names; a tap on a dark subpatch — or on a dark boundary
        // port of a live one — is dropped (silence).
        for o in &self.outputs {
            if dark.contains(&o.node) {
                continue;
            }
            let Some((key, port, _, _)) =
                resolve_output(&faces, &by_addr, &o.node, &o.port, Some(&o.port))?
            else {
                continue;
            };
            match o.channel {
                Some(channel) => graph.tap_output_channel(key, port, channel),
                None => graph.tap_output(key, port),
            }
        }

        // `interface`: the engine-honored I/O boundary (ADR-0032). Each external name resolves to
        // one internal `(node, port)`, direction-checked — an `inputs` name to an input port, an
        // `outputs` name to an output port (sole-output sugar allowed). Stored on the Graph for a
        // host Voicer or a nesting parent to bind; no Arg-type check here (the boundary inherits
        // the inner port's type — ADR-0034 §4 — so the consumer's wire check decides). An entry
        // may point at a subpatch's boundary port (re-export): it resolves through the face to
        // the same inner `(node, port)`. An entry whose target is dark — an unavailable subpatch,
        // or a dark boundary port of a live one — is dropped **and recorded** on the resolved
        // interface's dark sets, with a warning: the boundary port vanishes with the unavailable
        // child, but a consumer one level up degrades the same way instead of hitting a fatal
        // unknown-port error (dark degradation is transitive, ADR-0016).
        if let Some(iface) = &self.interface {
            let mut interface = Interface::default();
            let mut go_dark = |set: &mut BTreeSet<String>, name: &String, reference: &str| {
                set.insert(name.clone());
                warnings.push(LoadWarning::DarkInterfaceEntry {
                    name: name.clone(),
                    target: reference.to_string(),
                });
            };
            for (name, entry) in &iface.inputs {
                let reference = entry.target();
                let (src_addr, port) = parse_wire(reference);
                if dark.contains(src_addr) {
                    go_dark(&mut interface.dark_inputs, name, reference);
                    continue;
                }
                // An input ref must name its port explicitly — there is no sole-input sugar.
                let port_name = port.ok_or_else(|| LoadError::UnknownPort {
                    node: src_addr.to_string(),
                    port: reference.to_string(),
                })?;
                let Some((key, idx, _)) = resolve_input(&faces, &by_addr, src_addr, port_name)?
                else {
                    go_dark(&mut interface.dark_inputs, name, reference);
                    continue;
                };
                if let Some(m) = entry.meta() {
                    check_interface_override(name, m, &graph.nodes[key], idx, false)?;
                }
                interface.inputs.insert(name.clone(), (key, idx));
            }
            for (name, entry) in &iface.outputs {
                let reference = entry.target();
                let (src_addr, port) = parse_wire(reference);
                if dark.contains(src_addr) {
                    go_dark(&mut interface.dark_outputs, name, reference);
                    continue;
                }
                let Some((key, idx, _, _)) =
                    resolve_output(&faces, &by_addr, src_addr, reference, port)?
                else {
                    go_dark(&mut interface.dark_outputs, name, reference);
                    continue;
                };
                if let Some(m) = entry.meta() {
                    check_interface_override(name, m, &graph.nodes[key], idx, true)?;
                }
                interface.outputs.insert(name.clone(), (key, idx));
            }
            graph.interface = interface;
        }

        Ok(Loaded { graph, warnings })
    }

    /// Derive a document from a built [`Graph`] (the canonical "save" path). Nodes are emitted in
    /// a stable order, and within a node `config`/`inputs` keys are sorted (BTreeMap), so output is
    /// deterministic. A `Constant` override goes to `config`; a materialized `Float` override, an
    /// `Enum` choice (as its symbol), and every inbound wire go to `inputs` (ADR-0035).
    pub fn from_graph(graph: &Graph, instrument: impl Into<String>) -> Self {
        let mut nodes: Vec<NodeDoc> = graph
            .nodes
            .iter()
            .map(|(key, node)| {
                let d = &node.descriptor;
                let mut config: BTreeMap<String, ConfigValue> = BTreeMap::new();
                let mut inputs: BTreeMap<String, InputValue> = BTreeMap::new();

                // Constant overrides (ADR-0035) go to `config` — a non-default plan-time value the
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
                // Settable input overrides (ADR-0035) round-trip under the input's name: an `F32`
                // control as a number, an enum as its variant **symbol** (the primary wire form).
                for (port, arg) in &node.value_overrides {
                    let p = &d.inputs[*port];
                    let value = match doc_value(p, arg) {
                        DocValue::Number(n) => InputValue::Number(n),
                        DocValue::Symbol(s) => InputValue::Symbol(s),
                    };
                    inputs.insert(p.name.to_string(), value);
                }
                // Inbound wires: each edge whose destination is this node becomes a wire-ref, using
                // the sole-output sugar when the source has a single output.
                for c in graph.connections.iter().filter(|c| c.dst == key) {
                    let src = &graph.nodes[c.src];
                    let from = if src.descriptor.outputs.len() == 1 {
                        src.address.clone()
                    } else {
                        format!(
                            "{}.{}",
                            src.address, src.descriptor.outputs[c.src_port].name
                        )
                    };
                    inputs.insert(
                        d.inputs[c.dst_port].name.to_string(),
                        InputValue::Wire { from },
                    );
                }

                NodeDoc {
                    type_name: d.type_name.to_string(),
                    address: node.address.clone(),
                    doc: None,
                    config,
                    inputs,
                    // Logical resource ids round-trip from the ids stashed at build (ADR-0016
                    // `sample`, ADR-0032 `voice`). The decoded bytes/sub-graphs are bound
                    // out-of-band and are *not* reconstructed here — reload re-resolves from the id.
                    // No `patch`: a subpatch dissolves at build (ADR-0034 §2), so a built graph
                    // holds only the flattened equivalent — this save emits the inlined child
                    // nodes, not the reference. Reference-preserving save is the library thread
                    // (P7, #122); the *document*-level round-trip keeps `patch` via serde.
                    sample: node.sample_id.clone(),
                    voice: node.voice_id.clone(),
                    patch: None,
                    // Control metadata (ADR-0018) lives on the document, not the built Graph, so the
                    // save-from-graph path does not reconstruct it; document-level round-trip
                    // (load → re-serialize) preserves it via serde.
                    control: None,
                }
            })
            .collect();
        nodes.sort_by(|a, b| a.address.cmp(&b.address));

        let outputs = graph
            .outputs
            .iter()
            .map(|(key, port, channel)| PortRef {
                node: graph.nodes[*key].address.clone(),
                port: graph.nodes[*key].descriptor.outputs[*port].name.to_string(),
                channel: *channel,
            })
            .collect();

        // Reconstruct the `interface` boundary (ADR-0032) from its resolved `(node, port)` pairs,
        // emitting the canonical explicit `/node.port` form (never the sole-output sugar) so a
        // load → save → reload round-trip is stable. `None` when no boundary is declared.
        let iface = &graph.interface;
        let port_ref = |(key, port): &(crate::graph::NodeKey, usize), out: bool| {
            let n = &graph.nodes[*key];
            let pname = if out {
                n.descriptor.outputs[*port].name
            } else {
                n.descriptor.inputs[*port].name
            };
            format!("{}.{}", n.address, pname)
        };
        let interface = if iface.inputs.is_empty() && iface.outputs.is_empty() {
            None
        } else {
            Some(InterfaceDoc {
                // Bare targets only: presentational overrides (ADR-0034 §4) live on the
                // document, like `control` — the built Graph doesn't hold them, so the
                // save-from-graph path can't reconstruct them; the document-level round-trip
                // (load → re-serialize) preserves them via serde.
                inputs: iface
                    .inputs
                    .iter()
                    .map(|(name, np)| (name.clone(), port_ref(np, false).into()))
                    .collect(),
                outputs: iface
                    .outputs
                    .iter()
                    .map(|(name, np)| (name.clone(), port_ref(np, true).into()))
                    .collect(),
            })
        };

        Self {
            instrument: instrument.into(),
            doc: None,
            resources: BTreeMap::new(),
            interface,
            nodes,
            outputs,
        }
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

/// The synthesized boundary face of a `subpatch` node (ADR-0034 §4): one port per `interface`
/// name of the resolved child, each carrying the **inner** port's [`PortType`] verbatim (type
/// inherited, never overridable) and the inlined `(node, port)` it resolves to. An owned-string
/// artifact computed at build — deliberately *not* the engine [`Descriptor`], whose names are
/// `&'static str` because operators are compile-time-registered (ADR-0024/0025). It exists only
/// long enough to resolve + type-check the parent's boundary wires and then drops with the build
/// scope; the runtime holds no synthesized descriptor at all (what keeps §2's "zero runtime cost"
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
    /// (ADR-0016) instead of erroring. The sole-output sugar counts dark ports as real, so
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
/// subpatch boundary input checked against the **inner** port its face names (ADR-0034 §1):
/// a number needs a materialized `Float` or an enum, a symbol needs an enum, and the symbol must
/// name a variant (ADR-0028: an unknown symbol is an error, never a silent default).
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
            if desc.materialized_input(port_name).is_none() && desc.enum_input(port_name).is_none()
            {
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
            Ok(Some(Arg::Str(s.clone())))
        }
    }
}

/// Resolve a wire/tap/interface endpoint's **input** side to the inner `(node, port, type)`:
/// through the subpatch's synthesized boundary face when `addr` names one (ADR-0034 §4), else
/// through the document node's descriptor. The one statement of face-vs-descriptor input
/// resolution — every pass that lands on an input goes through here, so errors are labeled the
/// same way everywhere: the address and port name the author wrote, never a prefixed internal.
/// `Ok(None)` means the name is a **dark** boundary port (declared by the child, unresolvable
/// this load, ADR-0016) — the caller drops the reference; an unknown name stays fatal.
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
/// arm labels with the **boundary** port name (ADR-0034 §4's "errors speak in boundary terms").
/// Errors name `addr` — the node being resolved — in both arms; `Ok(None)` is a dark boundary
/// port (see [`resolve_input`]).
fn resolve_output(
    faces: &BTreeMap<String, BoundaryFace>,
    by_addr: &BTreeMap<String, (crate::graph::NodeKey, Descriptor)>,
    addr: &str,
    reference: &str,
    port: Option<&str>,
) -> Result<Option<(crate::graph::NodeKey, usize, PortType, String)>, LoadError> {
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

/// Inline a resolved subpatch child into `parent` (ADR-0034 §2–§4): move every child node in
/// under `prefix` (its address becomes `<prefix><child address>`, compounding for deeper nests),
/// remap the child's edges onto the new keys, and synthesize the boundary face from the child's
/// resolved `interface`. Prefixing is a pure naming transform — edges are `NodeKey`-resolved, so
/// no wiring can break (§3) — and per-reuse state isolation is automatic: each call splices a
/// freshly built child, so two reuses share no keys. A post-prefix address already claimed in
/// `addresses` is the fatal [`LoadError::DuplicateAddress`] (§3). The child's master `outputs`
/// taps do **not** cross the boundary — the `interface` is the whole contract (§4); a nested
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

    // Synthesize the face (§4): type inherited verbatim from the inner port the interface names.
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

/// A single override [`Arg`] in document-facing form (ADR-0035): a number for `F32`/`I32`, the
/// variant **symbol** for an enum choice (the primary wire form, ADR-0028).
#[derive(Debug, Clone, PartialEq)]
pub enum DocValue {
    Number(f64),
    Symbol(String),
}

/// The one Arg→document-value mapping, shared by [`InstrumentDoc::from_graph`] (save: `config`
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

/// Enforce the presentational-override law (ADR-0034 §4) on one resolved `interface` entry:
/// overrides decorate presentation but must stay **truthful**, because `describe` publishes them
/// as the boundary contract and no engine path reconciles them with the range the engine actually
/// clamps to. A `min`/`max` override must land on a port that has a numeric range, stay within
/// the engine-enforced bounds, not invert, and keep the effective default (the child's own
/// literal, else the descriptor default) inside the range it advertises. `label`/`unit`/`widget`
/// are unconstrained — they rename, they cannot lie about a value the engine will accept.
fn check_interface_override(
    name: &str,
    meta: &InterfaceMeta,
    node: &crate::graph::Node,
    port_idx: usize,
    output: bool,
) -> Result<(), LoadError> {
    if meta.min.is_none() && meta.max.is_none() {
        return Ok(());
    }
    let err = |reason: String| LoadError::InterfaceOverride {
        name: name.to_string(),
        reason,
    };
    let d = &node.descriptor;
    let p = if output {
        &d.outputs[port_idx]
    } else {
        &d.inputs[port_idx]
    };
    // The engine-enforced range: a swept scalar's F32Meta, or an integer port's I32 meta.
    let (inner_min, inner_max, inner_default) = match (&p.meta, &p.ty) {
        (Some(m), _) => (
            widen_f32(m.min),
            widen_f32(m.max),
            Some(widen_f32(m.default)),
        ),
        (None, PortType::I32 { meta: Some(m) }) => {
            (m.min as f64, m.max as f64, Some(m.default as f64))
        }
        _ => {
            return Err(err(format!(
                "range override on inner port {:?}, which has no numeric range",
                p.name
            )))
        }
    };
    for (bound, value) in [("min", meta.min), ("max", meta.max)] {
        if let Some(v) = value {
            if v < inner_min || v > inner_max {
                return Err(err(format!(
                    "{bound} {v} is outside the engine-enforced range [{inner_min}..{inner_max}] \
                     — the advertised range must be a subset of what the engine accepts"
                )));
            }
        }
    }
    let lo = meta.min.unwrap_or(inner_min);
    let hi = meta.max.unwrap_or(inner_max);
    if lo >= hi {
        return Err(err(format!(
            "advertised range [{lo}..{hi}] is inverted or empty"
        )));
    }
    // Inputs only: the effective default is what an unwired host actually gets — it must sit
    // inside the range a generated control will span.
    if !output {
        let effective = node
            .value_overrides
            .iter()
            .find(|(i, _)| *i == port_idx)
            .and_then(|(_, a)| match a {
                Arg::F32(v) => Some(widen_f32(*v)),
                Arg::I32(v) => Some(*v as f64),
                _ => None,
            })
            .or(inner_default);
        if let Some(v) = effective {
            if v < lo || v > hi {
                return Err(err(format!(
                    "effective default {v} is outside the advertised range [{lo}..{hi}]"
                )));
            }
        }
    }
    Ok(())
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
    /// has no external OSC form (the boundary opt-out — converters are issue #146), so the wire
    /// could never send anything and is rejected at load, not left silently dead.
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
        let doc = InstrumentDoc::from_json(TWO_NODE).expect("parse");
        let reparsed = InstrumentDoc::from_json(&doc.to_json_pretty()).expect("reparse");
        assert_eq!(doc, reparsed);
    }

    #[test]
    fn control_block_survives_doc_round_trip() {
        // ADR-0018: `control` is opaque passthrough — the engine ignores it, but it must
        // survive load -> re-serialize so a surface generator can read what it wrote.
        let json = r#"{"instrument":"t",
            "nodes":[{"type":"map_f32_signal","address":"/brightness",
                      "control":{"label":"Brightness","widget":"fader","unit":"%"}}]}"#;
        let doc = InstrumentDoc::from_json(json).expect("parse");
        let ctl = doc.nodes[0]
            .control
            .as_ref()
            .expect("control preserved on load");
        assert_eq!(ctl["label"], "Brightness");
        let reparsed = InstrumentDoc::from_json(&doc.to_json_pretty()).expect("reparse");
        assert_eq!(doc, reparsed, "control block must round-trip unchanged");
    }

    #[test]
    fn from_graph_then_build_is_stable() {
        // load -> save -> reparse -> save again: the two saved docs are identical.
        let g1 = load(TWO_NODE, &reg()).expect("load");
        let saved1 = InstrumentDoc::from_graph(&g1, "test");
        let g2 = saved1.build(&reg()).expect("rebuild");
        let saved2 = InstrumentDoc::from_graph(&g2, "test");
        assert_eq!(saved1, saved2);
        assert_eq!(saved1.nodes.len(), 2);
    }

    #[test]
    fn value_overrides_round_trip_across_types() {
        // ADR-0035: the collapsed `value_overrides` channel must save an `F32` control override as a
        // number and an enum override as its variant **symbol** (reconstructed via
        // `EnumMeta::symbol_of`, not a stored index) — one channel, two on-disk forms.
        let json = r#"{"instrument":"t","nodes":[
            {"type":"filter","address":"/f","inputs":{"mode":"Hp","cutoff":3000}}]}"#;
        let g = load(json, &reg()).expect("load");
        let doc = InstrumentDoc::from_graph(&g, "t");
        let inputs = &doc.nodes[0].inputs;
        assert_eq!(inputs["cutoff"], InputValue::Number(3000.0));
        assert_eq!(inputs["mode"], InputValue::Symbol("Hp".to_string()));
    }

    // ADR-0032 §1 — the `interface` block. A voice-shaped patch: osc.freq / env.gate in,
    // osc.audio / env.active out. `/env` has two outputs so a sole-output ref would be ambiguous;
    // the explicit `/env.active` resolves it.
    const VOICE_IFACE: &str = r#"{
        "instrument": "voice",
        "interface": {
            "inputs":  { "freq": "/osc.freq", "gate": "/env.gate" },
            "outputs": { "audio": "/osc.audio", "active": "/env.active" }
        },
        "nodes": [
            { "type": "oscillator", "address": "/osc" },
            { "type": "envelope", "address": "/env" }
        ]
    }"#;

    #[test]
    fn interface_block_resolves_to_internal_ports() {
        let g = load(VOICE_IFACE, &reg()).expect("load");
        let osc = g.find("/osc").unwrap();
        let env = g.find("/env").unwrap();
        let osc_d = &g.nodes[osc].descriptor;
        let env_d = &g.nodes[env].descriptor;

        // inputs resolve to the right node + input port index, direction-checked.
        assert_eq!(
            g.interface.inputs["freq"],
            (
                osc,
                osc_d.inputs.iter().position(|p| p.name == "freq").unwrap()
            )
        );
        assert_eq!(
            g.interface.inputs["gate"],
            (
                env,
                env_d.inputs.iter().position(|p| p.name == "gate").unwrap()
            )
        );
        // outputs resolve to the right node + output port index (explicit `/env.active`).
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

    // ADR-0034 §4 — the object entry form: target + presentational overrides.
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
    fn interface_entry_object_form_resolves_like_the_bare_form() {
        // The object form's `target` resolves exactly as a bare string entry would (§4:
        // overrides decorate presentation; they never change what resolves or what type flows).
        let g = load(VOICE_IFACE_META, &reg()).expect("load");
        let osc = g.find("/osc").unwrap();
        let freq = g.nodes[osc]
            .descriptor
            .inputs
            .iter()
            .position(|p| p.name == "freq")
            .unwrap();
        assert_eq!(g.interface.inputs["freq"], (osc, freq));
    }

    #[test]
    fn interface_entry_overrides_round_trip_through_the_document() {
        // Overrides live on the document (like `control`): serde round-trip preserves them.
        let doc = InstrumentDoc::from_json(VOICE_IFACE_META).expect("parse");
        let meta = doc.interface.as_ref().unwrap().inputs["freq"]
            .meta()
            .expect("object form carries overrides");
        assert_eq!(meta.target, "/osc.freq");
        assert_eq!(meta.label.as_deref(), Some("Pitch"));
        assert_eq!(meta.unit.as_deref(), Some("Hz"));
        assert_eq!(meta.widget.as_deref(), Some("knob"));
        assert_eq!((meta.min, meta.max), (Some(50.0), Some(2000.0)));
        let reparsed = InstrumentDoc::from_json(&doc.to_json_pretty()).expect("reparse");
        assert_eq!(doc, reparsed);
    }

    #[test]
    fn interface_entry_type_override_is_rejected() {
        // §4: the Arg type is inherited and NOT overridable — the object form has no field to
        // express one, so an attempt fails to parse (never a silently ignored key), and the
        // error names the offending field.
        let json = r#"{"instrument":"t","interface":{
            "inputs":{"freq":{"target":"/osc.freq","type":"note"}}},
            "nodes":[{"type":"oscillator","address":"/osc"}]}"#;
        let err = match load(json, &reg()) {
            Err(e @ LoadError::Json(_)) => e,
            Err(e) => panic!("expected Json error, got {e:?}"),
            Ok(_) => panic!("type override must not load"),
        };
        assert!(
            err.to_string().contains("unknown field `type`"),
            "error must name the rejected field: {err}"
        );
    }

    #[test]
    fn interface_entry_errors_name_the_offending_field() {
        // Hand-dispatched entry parsing (string vs object) keeps InterfaceMeta's pointed serde
        // errors; `#[serde(untagged)]` would collapse all of these into one opaque
        // "did not match any variant" message.
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
            (r#"{"label":"Pitch"}"#, "missing field `target`"),
            (
                r#"{"target":"/osc.freq","min":"200"}"#,
                "invalid type: string",
            ),
            (r#"true"#, "target string or an override object"),
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

    // ADR-0034 §4 override law (review F1/F5/F6): a range override must stay a subset of the
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
        let doc = InstrumentDoc::from_json(VOICE_IFACE).expect("parse");
        let reparsed = InstrumentDoc::from_json(&doc.to_json_pretty()).expect("reparse");
        assert_eq!(doc, reparsed);
        // ...and from_graph reconstructs an equivalent, stable interface (canonical `/node.port`).
        let g = load(VOICE_IFACE, &reg()).expect("load");
        let saved = InstrumentDoc::from_graph(&g, "voice");
        let iface = saved.interface.as_ref().expect("interface reconstructed");
        assert_eq!(iface.inputs["freq"].target(), "/osc.freq");
        assert_eq!(iface.outputs["active"].target(), "/env.active");
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

    // ADR-0032 §2 — the instrument-kind resource. A resolver whose `resolve_text` returns a voice
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
        assert_eq!(loaded.graph.nodes.len(), 2);
        // The sub-Graph carries its resolved interface, ready for a host to bind.
        assert!(loaded.graph.interface.inputs.contains_key("freq"));
        assert!(loaded.graph.interface.outputs.contains_key("audio"));
    }

    #[test]
    fn instrument_resource_resolve_failure_is_a_warning_not_fatal() {
        // A resolver that can't produce the text (the default `resolve_text`): non-fatal per
        // ADR-0016 — an empty graph plus a ResolveFailed warning, never a hard error.
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
        // matching ADR-0016: availability problems warn, wiring problems error.
        const BROKEN: &str = r#"{"instrument":"v","nodes":[{"type":"nope","address":"/x"}]}"#;
        assert!(matches!(
            resolve_instrument("v.json", &reg(), &PatchResolver(BROKEN)),
            Err(LoadError::UnknownType { .. })
        ));
    }

    // ADR-0034 (nesting P4) — a `subpatch` node references an instrument patch; at build the
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
        // The P4 acceptance shape (ADR-0034 §2–§3): the child's nodes are spliced in under the
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
            "the subpatch node dissolves at build (§2)"
        );
        assert!(loaded.graph.find("/sub/osc").is_some());
        assert!(loaded.graph.find("/sub/env").is_some());
        assert_eq!(
            loaded.graph.nodes.len(),
            2,
            "child nodes only, no host node"
        );
    }

    #[test]
    fn boundary_wires_and_literals_resolve_through_the_face() {
        // ADR-0034 §4–§5: a wire out of `/sub.audio` lands directly on the inner `(node, port)`
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

        // The boundary literal seeded the inner oscillator's freq override.
        let (freq_port, _) = g.nodes[osc].descriptor.materialized_input("freq").unwrap();
        assert!(
            g.nodes[osc]
                .value_overrides
                .iter()
                .any(|(p, a)| *p == freq_port && *a == Arg::F32(220.0)),
            "boundary literal lands as the inner value-override: {:?}",
            g.nodes[osc].value_overrides
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
        // The face inherits the inner port's type verbatim (§4), so the ordinary pass-2 check
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
        // Signal→Value is a hard error with no implicit sample-and-hold (ADR-0031). The load-time
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

    // P5 (ADR-0034 §5): the adversarial boundary-type matrix. Every case is a **well-typed inner
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
        // variant is BadInputValue named at the boundary (ADR-0028: never snaps to default).
        const FILTER_CHILD: &str = r#"{
            "instrument": "fx",
            "interface": { "inputs": { "mode": "/f.mode" } },
            "nodes": [ { "type": "filter", "address": "/f" } ]
        }"#;
        let ok = r#"{"instrument":"p","resources":{"v":"v.json"},"nodes":[
            {"type":"subpatch","address":"/sub","patch":"v","inputs":{"mode":"Hp"}}]}"#;
        let loaded = load_instrument(ok, &reg(), &PatchResolver(FILTER_CHILD)).expect("load");
        let f = loaded.graph.find("/sub/f").unwrap();
        assert!(
            !loaded.graph.nodes[f].value_overrides.is_empty(),
            "symbol literal seeds the inner enum override"
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
    fn post_prefix_address_collision_is_fatal() {
        // ADR-0034 §3: a child address that, after prefixing, collides with an existing parent
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
        // Two levels (§3): the middle patch nests a child and re-exports its boundary input as
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
                            "nodes":[{"type":"oscillator","address":"/osc"}]}"#
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
        // The outer boundary literal flowed through the re-export to the innermost oscillator.
        let (freq_port, _) = g.nodes[osc].descriptor.materialized_input("freq").unwrap();
        assert!(g.nodes[osc]
            .value_overrides
            .iter()
            .any(|(p, a)| *p == freq_port && *a == Arg::F32(330.0)));
        // And the boundary wire chain resolved to one ordinary edge onto /out.
        assert_eq!(g.connections.len(), 1);
    }

    #[test]
    fn wiring_an_internally_driven_boundary_input_is_fatal() {
        // The child drives its exposed `audio` input with its own internal wire; the plan reads
        // exactly one inbound edge per Signal input, so a parent wire onto `/sub.audio` would
        // load clean and do nothing. Reject it loud, in boundary terms.
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
        assert!(matches!(
            load_instrument(json, &reg(), &PatchResolver(DRIVEN_CHILD)),
            Err(LoadError::BoundaryInputDriven { node, input })
                if node == "/sub" && input == "audio"
        ));
        // The same boundary input left unwired inside the child accepts the parent's wire.
        const OPEN_CHILD: &str = r#"{
            "instrument": "fx",
            "interface": { "inputs": { "audio": "/out.audio" } },
            "nodes": [ { "type": "output", "address": "/out" } ]
        }"#;
        let loaded = load_instrument(json, &reg(), &PatchResolver(OPEN_CHILD)).expect("load");
        assert_eq!(loaded.graph.connections.len(), 1);
    }

    #[test]
    fn missing_grandchild_degrades_dark_transitively() {
        // The review-repro shape: mid re-exports its whole interface from a leaf whose file is
        // missing. Mid loads with a warning and records `freq`/`audio` as **dark** interface
        // entries; the parent's boundary literal, wire, and tap onto them then degrade exactly
        // like references to a dark nest — dropped with the warning — instead of escalating to
        // a fatal UnknownInput/UnknownPort one level up (ADR-0016: dark is transitive).
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
        assert!(
            loaded.warnings.iter().any(|w| matches!(unwrap_nested(w),
                    LoadWarning::DarkInterfaceEntry { name, .. } if name == "freq")),
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
        // The interface is the whole contract (§4): a child's own master `outputs` taps vanish on
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
            "nodes":[{"type":"sample","address":"/s","sample":"kick"}]}"#;
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
        // A `patch` id absent from the `resources` table degrades to a warning (ADR-0016): the
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
        // The warning names the slot that actually failed — a `patch` ref, not a sample.
        assert!(matches!(
            loaded.warnings.as_slice(),
            [LoadWarning::MissingResource { slot: "patch", .. }]
        ));
        assert_eq!(
            loaded.warnings[0].to_string(),
            r#"node "/sub": patch "absent" not in resources table"#
        );
    }

    #[test]
    fn subpatch_resolve_failure_warns_and_dissolves_dark() {
        // The id resolves to a source, but `resolve_text` fails (the default `Failing` resolver):
        // non-fatal per ADR-0016 — a ResolveFailed warning, never a hard error. Like the
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
        // Each reuse still gets its own inlined child — disjoint prefixes, disjoint state (§3).
        assert!(loaded.graph.find("/one/osc").is_some());
        assert!(loaded.graph.find("/two/osc").is_some());
    }

    #[test]
    fn subpatch_structural_error_is_fatal() {
        // The sub-patch resolves but is structurally broken (unknown operator type): fatal, matching
        // the voice path — availability warns, structure/wiring errors (ADR-0016/0034).
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
        // The ADR-0016 split lands at the fetch seam (ADR-0034 §1): once the text is in hand,
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
        let doc = InstrumentDoc::from_json(PARENT_WITH_SUBPATCH).expect("parse");
        let reparsed = InstrumentDoc::from_json(&doc.to_json_pretty()).expect("reparse");
        assert_eq!(doc, reparsed);
        assert_eq!(reparsed.nodes[0].patch.as_deref(), Some("myvoice"));
    }

    #[test]
    fn from_graph_saves_the_flattened_equivalent() {
        // ADR-0034 §2: the subpatch dissolves at build, so saving a built graph emits the inlined
        // child nodes under their prefixed addresses — no `subpatch` node, no `patch` ref. The
        // deliberate P4 shape; reference-preserving save is P7 (#122).
        let loaded = load_instrument(PARENT_WITH_SUBPATCH, &reg(), &PatchResolver(VOICE_IFACE))
            .expect("load");
        let saved = InstrumentDoc::from_graph(&loaded.graph, "p");
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
        let saved = InstrumentDoc::from_graph(&g, "t");
        let v = saved.nodes.iter().find(|n| n.address == "/v").unwrap();
        assert!(matches!(
            v.config.get("voices"),
            Some(ConfigValue::Number(_))
        ));
        assert!(!v.inputs.contains_key("voices"));
    }
}
