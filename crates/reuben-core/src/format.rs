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

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::descriptor::{Descriptor, PortType};
use crate::graph::{Graph, Interface};
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
    pub inputs: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
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
    /// must be a declared [`Constant`](Descriptor::constant_param); a runtime input set here, or a
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
    /// Public-control metadata for a generated control surface (ADR-0018): marks this node as
    /// player-facing and carries display hints (`label`, optional `unit`/`widget`/range). The
    /// engine never reads it — it is passed through opaquely so it survives load → round-trip →
    /// re-serialize (serde would otherwise drop an unknown field, erasing it on `from_graph`).
    /// A control-surface generator reads it; `None` means the node is internal plumbing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control: Option<serde_json::Value>,
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
    /// A `config` name is not a declared [`Constant`](Descriptor::constant_param).
    UnknownConfig { node: String, name: String },
    /// A `Constant` (e.g. `voices`) appears in `inputs` — it must live in `config`, since changing
    /// it would rebuild the graph (ADR-0028).
    ConstantInInputs { node: String, name: String },
    /// A wire-ref uses the sole-output sugar (`"/node"`) but the source has more than one output,
    /// so the intended port is ambiguous.
    AmbiguousWire { node: String, reference: String },
    /// A wire joins two ports of incompatible [`PortType`]s (e.g. `Note` → `Buffer`) — the illegal
    /// wiring (ADR-0030). Equal types are fine, and an `F32` source into a `Buffer` port is the one
    /// implicit ZOH bridge; everything else is rejected here.
    TypeMismatch {
        from: String,
        from_type: Box<PortType>,
        to: String,
        to_type: Box<PortType>,
    },
    /// A node carries a `sample` reference but its operator declares no such resource slot
    /// (ADR-0016) — a structural misuse, fatal like the other wiring errors.
    UnknownResource { node: String, slot: String },
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
            } => write!(
                f,
                "wire {from} ({from_type:?}) -> {to} ({to_type:?}) joins ports of different Arg types"
            ),
            LoadError::UnknownResource { node, slot } => {
                write!(f, "node {node:?} has no resource slot {slot:?}")
            }
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
/// players to nothing (they play silence). Use [`load_instrument`] to resolve and bind
/// decoded audio.
pub fn load(json: &str, registry: &Registry) -> Result<Graph, LoadError> {
    InstrumentDoc::from_json(json)?.build(registry)
}

/// A non-fatal resource problem found at load (ADR-0016). The owning node still builds and
/// binds to an empty buffer (so it plays silence); the boundary surfaces these to the user
/// because they are authoring errors, just not crashing ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadWarning {
    /// A node names a resource id absent from the `resources` table.
    MissingResource { node: String, id: String },
    /// A resource id resolves to a source that could not be loaded/decoded.
    ResolveFailed {
        id: String,
        source: String,
        reason: String,
    },
}

impl fmt::Display for LoadWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadWarning::MissingResource { node, id } => {
                write!(f, "node {node:?}: sample {id:?} not in resources table")
            }
            LoadWarning::ResolveFailed { id, source, reason } => {
                write!(f, "sample {id:?} ({source:?}): {reason}")
            }
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
    let doc = InstrumentDoc::from_json(json)?;
    let mut graph = doc.build(registry)?;
    let mut warnings = Vec::new();

    // Resolve every referenced id once into the store; record id -> handle for binding.
    let mut store = ResourceStore::new();
    let mut handles: BTreeMap<String, SampleId> = BTreeMap::new();
    for n in &doc.nodes {
        let Some(id) = &n.sample else { continue };
        if handles.contains_key(id) {
            continue; // dedup: already resolved by an earlier node
        }
        let buffer = match doc.resources.get(id) {
            None => {
                warnings.push(LoadWarning::MissingResource {
                    node: n.address.clone(),
                    id: id.clone(),
                });
                SampleBuffer::empty()
            }
            Some(source) => match resolver.resolve(source) {
                Ok(b) => b,
                Err(e) => {
                    warnings.push(LoadWarning::ResolveFailed {
                        id: id.clone(),
                        source: source.clone(),
                        reason: e.to_string(),
                    });
                    SampleBuffer::empty()
                }
            },
        };
        handles.insert(id.clone(), store.insert(id.clone(), buffer));
    }

    let store = Arc::new(store);

    // Bind each resource-bearing node's Lane-0 op (spawn carries it to the other Voices).
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
        match doc.resources.get(id) {
            None => {
                warnings.push(LoadWarning::MissingResource {
                    node: n.address.clone(),
                    id: id.clone(),
                });
                for _ in 0..n_voices {
                    voices.push(Graph::new());
                }
            }
            Some(source) => {
                for i in 0..n_voices {
                    let loaded = resolve_instrument(source, registry, resolver)?;
                    // One copy's warnings suffice — the N builds are identical.
                    if i == 0 {
                        warnings.extend(loaded.warnings);
                    }
                    voices.push(loaded.graph);
                }
            }
        }
        graph.nodes[key].op.bind_voices(voices);
    }

    Ok(Loaded { graph, warnings })
}

/// The voice-pool size for a Voicer node (ADR-0032): its `voices` config constant, else the
/// descriptor's `voices` param default, floored to 1.
fn voice_count(n: &NodeDoc, descriptor: &Descriptor) -> usize {
    n.config
        .get("voices")
        .and_then(|v| match v {
            ConfigValue::Number(x) => Some(*x),
            ConfigValue::Symbol(_) => None,
        })
        .or_else(|| {
            descriptor
                .params
                .iter()
                .find(|p| p.name == "voices")
                .map(|p| p.default as f64)
        })
        .map(|x| (x.round() as i64).max(1) as usize)
        .unwrap_or(1)
}

/// Resolve an **instrument-kind resource** (ADR-0032 §2): a patch `source` (a path) is read to its
/// JSON via [`ResourceResolver::resolve_text`], then built into a sub-[`Graph`] through the full
/// [`load_instrument`] path — so the sub-patch's own `sample` resources resolve recursively and its
/// `interface` boundary is resolved for the host to bind. Structural/wiring problems in the patch
/// are fatal ([`LoadError`]); a `resolve_text` failure is **non-fatal** (ADR-0016): it yields an
/// empty graph plus a [`LoadWarning::ResolveFailed`], so one missing voice patch never crashes the
/// host. This is the net-new piece ADR-0032 needs — "a resource that is a Graph, not bytes."
pub fn resolve_instrument(
    source: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<Loaded, LoadError> {
    match resolver.resolve_text(source) {
        Ok(text) => load_instrument(&text, registry, resolver),
        Err(e) => Ok(Loaded {
            graph: Graph::new(),
            warnings: vec![LoadWarning::ResolveFailed {
                id: source.to_string(),
                source: source.to_string(),
                reason: e.to_string(),
            }],
        }),
    }
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
    /// type-checking each `Arg` type (ADR-0030).
    pub fn build(&self, registry: &Registry) -> Result<Graph, LoadError> {
        let mut graph = Graph::new();
        // address -> (key, descriptor) for resolving wire-refs and outputs.
        let mut by_addr: BTreeMap<&str, (crate::graph::NodeKey, Descriptor)> = BTreeMap::new();

        // Pass 1: nodes, config constants, literal inputs.
        for n in &self.nodes {
            let entry = registry
                .get(&n.type_name)
                .ok_or_else(|| LoadError::UnknownType {
                    address: n.address.clone(),
                    type_name: n.type_name.clone(),
                })?;
            if by_addr.contains_key(n.address.as_str()) {
                return Err(LoadError::DuplicateAddress(n.address.clone()));
            }
            let descriptor = entry.descriptor.clone();
            // A `sample` ref is only valid on an operator that declares the slot (ADR-0016).
            if n.sample.is_some() && !descriptor.has_resource("sample") {
                return Err(LoadError::UnknownResource {
                    node: n.address.clone(),
                    slot: "sample".to_string(),
                });
            }
            // A `voice` instrument-resource ref likewise requires the `voice` slot (ADR-0032 §2).
            if n.voice.is_some() && !descriptor.has_resource("voice") {
                return Err(LoadError::UnknownResource {
                    node: n.address.clone(),
                    slot: "voice".to_string(),
                });
            }
            let key = graph.add_boxed(&n.address, (entry.make)(), descriptor.clone());
            // Retain the logical resource ids so `from_graph` round-trips the reference on save
            // (the resolved bytes/sub-graphs are bound out-of-band and do not survive the build).
            graph.nodes[key].sample_id = n.sample.clone();
            graph.nodes[key].voice_id = n.voice.clone();

            // `config`: every name must be a declared Constant; apply it at the param slot the
            // lane rule reads (ADR-0028).
            for (name, value) in &n.config {
                if !descriptor.is_constant_param(name) {
                    return Err(LoadError::UnknownConfig {
                        node: n.address.clone(),
                        name: name.clone(),
                    });
                }
                match value {
                    ConfigValue::Number(v) => graph.set_param(key, name, *v as f32),
                    ConfigValue::Symbol(s) => graph.set_enum(key, name, s),
                }
            }

            // `inputs`: a Constant here is an error; literals apply now, wire-refs in pass 2.
            for (name, value) in &n.inputs {
                if descriptor.is_constant_param(name) {
                    return Err(LoadError::ConstantInInputs {
                        node: n.address.clone(),
                        name: name.clone(),
                    });
                }
                match value {
                    InputValue::Wire { .. } => {} // pass 2
                    InputValue::Number(v) => {
                        if descriptor.param_index(name).is_none()
                            && descriptor.materialized_input(name).is_none()
                            && descriptor.enum_input(name).is_none()
                        {
                            return Err(LoadError::UnknownInput {
                                node: n.address.clone(),
                                input: name.clone(),
                            });
                        }
                        graph.set_param(key, name, *v as f32);
                    }
                    InputValue::Symbol(s) => {
                        // An `Enum` symbol is only valid on an enum input, and must name a variant
                        // (ADR-0028: an unknown symbol is an error, never a silent default).
                        let Some((_, e)) = descriptor.enum_input(name) else {
                            return Err(LoadError::UnknownInput {
                                node: n.address.clone(),
                                input: name.clone(),
                            });
                        };
                        if e.resolve(s).is_none() {
                            return Err(LoadError::BadInputValue {
                                node: n.address.clone(),
                                input: name.clone(),
                                value: s.clone(),
                            });
                        }
                        graph.set_enum(key, name, s);
                    }
                }
            }

            by_addr.insert(&n.address, (key, descriptor));
        }

        // Pass 2: wire-refs -> edges (Arg-type-checked).
        for n in &self.nodes {
            let (dst_key, dst_desc) = lookup(&by_addr, &n.address)?;
            for (name, value) in &n.inputs {
                let InputValue::Wire { from } = value else {
                    continue;
                };
                let dst_port = in_port(dst_desc, &n.address, name)?;
                let (src_addr, src_port_name) = parse_wire(from);
                let (src_key, src_desc) = lookup(&by_addr, src_addr)?;
                let src_port = resolve_out_port(src_desc, &n.address, from, src_port_name)?;

                let from_ty = &src_desc.outputs[src_port].ty;
                let to_ty = &dst_desc.inputs[dst_port].ty;
                // Equal types wire directly. `F32` and `Buffer` interconvert (ADR-0030): an `F32`
                // source into a `Buffer` port ZOH-materializes, and a `Buffer` source into an `F32`
                // control port is shared and read per-sample via `io.signal` (the
                // `voicer.freq -> osc.freq` CV path). Anything else is illegal.
                let compatible = from_ty == to_ty
                    || matches!(
                        (from_ty, to_ty),
                        (PortType::F32, PortType::F32Buffer) | (PortType::F32Buffer, PortType::F32)
                    );
                if !compatible {
                    return Err(LoadError::TypeMismatch {
                        from: format!("{}.{}", src_addr, src_desc.outputs[src_port].name),
                        from_type: Box::new(from_ty.clone()),
                        to: format!("{}.{}", n.address, name),
                        to_type: Box::new(to_ty.clone()),
                    });
                }
                graph.connect(src_key, src_port, dst_key, dst_port);
            }
        }

        // `outputs`: master taps (ADR-0026).
        for o in &self.outputs {
            let (key, desc) = lookup(&by_addr, &o.node)?;
            let port = out_port(desc, o)?;
            match o.channel {
                Some(channel) => graph.tap_output_channel(key, port, channel),
                None => graph.tap_output(key, port),
            }
        }

        // `interface`: the engine-honored I/O boundary (ADR-0032). Each external name resolves to
        // one internal `(node, port)`, direction-checked — an `inputs` name to an input port, an
        // `outputs` name to an output port (sole-output sugar allowed). Stored on the Graph for the
        // host Voicer to bind; no Arg-type check here (the host's contract decides port types).
        if let Some(iface) = &self.interface {
            let mut interface = Interface::default();
            for (name, reference) in &iface.inputs {
                let (src_addr, port) = parse_wire(reference);
                let (key, desc) = lookup(&by_addr, src_addr)?;
                // An input ref must name its port explicitly — there is no sole-input sugar.
                let port_name = port.ok_or_else(|| LoadError::UnknownPort {
                    node: src_addr.to_string(),
                    port: reference.clone(),
                })?;
                let idx = in_port(desc, src_addr, port_name)?;
                interface.inputs.insert(name.clone(), (key, idx));
            }
            for (name, reference) in &iface.outputs {
                let (src_addr, port) = parse_wire(reference);
                let (key, desc) = lookup(&by_addr, src_addr)?;
                let idx = resolve_out_port(desc, src_addr, reference, port)?;
                interface.outputs.insert(name.clone(), (key, idx));
            }
            graph.interface = interface;
        }

        Ok(graph)
    }

    /// Derive a document from a built [`Graph`] (the canonical "save" path). Nodes are emitted in
    /// a stable order, and within a node `config`/`inputs` keys are sorted (BTreeMap), so output is
    /// deterministic. A `Constant` param goes to `config`; a non-default param, a materialized
    /// `Float` override, an `Enum` choice (as its symbol), and every inbound wire go to `inputs`.
    pub fn from_graph(graph: &Graph, instrument: impl Into<String>) -> Self {
        let mut nodes: Vec<NodeDoc> = graph
            .nodes
            .iter()
            .map(|(key, node)| {
                let d = &node.descriptor;
                let mut config: BTreeMap<String, ConfigValue> = BTreeMap::new();
                let mut inputs: BTreeMap<String, InputValue> = BTreeMap::new();

                // Params: the Constant goes to `config`; others to `inputs` only when non-default
                // (defaults reload as defaults, keeping save minimal and round-trips stable).
                for (i, p) in d.params.iter().enumerate() {
                    if d.is_constant_param(p.name) {
                        config.insert(
                            p.name.to_string(),
                            ConfigValue::Number(node.params[i] as f64),
                        );
                    } else if node.params[i] != p.default {
                        inputs.insert(
                            p.name.to_string(),
                            InputValue::Number(node.params[i] as f64),
                        );
                    }
                }
                // Materialized `Float` input overrides (ADR-0028) — the unwired-default a literal
                // set — round-trip as the input's name.
                for &(port, v) in &node.input_overrides {
                    inputs.insert(
                        d.inputs[port].name.to_string(),
                        InputValue::Number(v as f64),
                    );
                }
                // `Enum` input overrides save as the variant **symbol** (the primary wire form).
                for &(port, idx) in &node.enum_overrides {
                    let sym = d.inputs[port]
                        .enum_meta()
                        .and_then(|e| e.variants.get(idx))
                        .copied()
                        .unwrap_or_default();
                    inputs.insert(
                        d.inputs[port].name.to_string(),
                        InputValue::Symbol(sym.to_string()),
                    );
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
                    sample: node.sample_id.clone(),
                    voice: node.voice_id.clone(),
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
                inputs: iface
                    .inputs
                    .iter()
                    .map(|(name, np)| (name.clone(), port_ref(np, false)))
                    .collect(),
                outputs: iface
                    .outputs
                    .iter()
                    .map(|(name, np)| (name.clone(), port_ref(np, true)))
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
    by_addr: &'a BTreeMap<&str, (crate::graph::NodeKey, Descriptor)>,
    node: &str,
) -> Result<(crate::graph::NodeKey, &'a Descriptor), LoadError> {
    by_addr
        .get(node)
        .map(|(k, d)| (*k, d))
        .ok_or_else(|| LoadError::UnknownNode(node.to_string()))
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
    dst_node: &str,
    reference: &str,
    port: Option<&str>,
) -> Result<usize, LoadError> {
    match port {
        Some(p) => desc
            .outputs
            .iter()
            .position(|o| o.name == p)
            .ok_or_else(|| LoadError::UnknownPort {
                node: dst_node.to_string(),
                port: p.to_string(),
            }),
        None if desc.outputs.len() == 1 => Ok(0),
        None => Err(LoadError::AmbiguousWire {
            node: dst_node.to_string(),
            reference: reference.to_string(),
        }),
    }
}

fn out_port(desc: &Descriptor, r: &PortRef) -> Result<usize, LoadError> {
    desc.outputs
        .iter()
        .position(|p| p.name == r.port)
        .ok_or_else(|| LoadError::UnknownPort {
            node: r.node.clone(),
            port: r.port.clone(),
        })
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
        let slot = g.nodes[key].descriptor.param_index("voices").unwrap();
        assert_eq!(g.nodes[key].params[slot], 3.0);
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
        assert_eq!(iface.inputs["freq"], "/osc.freq");
        assert_eq!(iface.outputs["active"], "/env.active");
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
