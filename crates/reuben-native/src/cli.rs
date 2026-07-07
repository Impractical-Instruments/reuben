//! CLI introspection for the Patcher skill (ADR-0020): `describe` the operator set and
//! `validate` an instrument without touching audio hardware.
//!
//! These back the `reuben describe` / `reuben validate` subcommands but are pure functions
//! over [`Registry`] + JSON so they test through real load/plan code paths, not a process.

use reuben_core::descriptor::{Curve, Descriptor, Port, PortType};
use reuben_core::format::{DocValue, InstrumentDoc, LoadError};
use reuben_core::plan::Plan;
use reuben_core::resources::ResourceResolver;
use reuben_core::{
    describe_boundary, load_instrument, load_instrument_doc, AudioConfig, BoundaryPortDesc,
    Registry,
};

use serde::Serialize;

/// One operator's self-description, flattened from its [`Descriptor`] for agent grounding.
#[derive(Debug, Serialize)]
pub struct OperatorInfo {
    pub type_name: String,
    /// The whole input surface as one list (mirrors [`Descriptor::inputs`] +
    /// [`constants`](Descriptor::constants)): runtime inputs first, then plan-time `Constant` ports
    /// marked `constant: true`. There is no separate `params`/`enums`/`constants` split — a port's
    /// `kind` and its optional metadata already say whether it is a scalar, integer, or enum.
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
}

/// One port, flattened from its [`Port`] for agent grounding. Inputs and outputs share this shape;
/// a plan-time [`Constant`](Descriptor::constants) (ADR-0035) is just an input with `constant: true`
/// — an immutable port set in a node's `config` block, never wired in `inputs`. Optional metadata
/// appears only where the port's type carries it: `default`/`min`/`max`/`unit`/`curve` for a swept
/// scalar, `default`/`min`/`max` for an integer, `default`/`variants` for an enum.
#[derive(Debug, Serialize)]
pub struct PortInfo {
    pub name: String,
    /// The port's [`PortType`] as the glossary's word: `"value"` (a held `f32` Value),
    /// `"signal"` (a dense `f32_buffer` Signal), `"int"`, `"enum"`, `"message"` (Note),
    /// `"harmony"` (Harmony), `"vocab"`, or `"string"`. The two numeric kinds are one wiring
    /// family with a single implicit bridge: `value` → `signal` materializes (ADR-0031);
    /// the reverse is a hard error.
    pub kind: String,
    /// A plan-time `Constant` (ADR-0035): set in the node's `config` block, not wired in `inputs`.
    /// Omitted (false) for an ordinary runtime input or any output.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub constant: bool,
    /// Unwired default: a number for a scalar/integer control, the variant symbol for an enum.
    /// Omitted for a port with no settable default (audio buffers, `Note`/`Harmony`, outputs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub unit: String,
    /// `"linear"` or `"exponential"` for a swept scalar; omitted for non-scalar ports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curve: Option<String>,
    /// The ordered enum choices (ADR-0030); empty for non-enum ports.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<String>,
    /// Display-name from an `interface` pipe entry (ADR-0034 §4 / ADR-0038). Only a boundary
    /// port carries one — operator ports have no label.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Widget hint from an `interface` pipe entry (ADR-0034 §4 / ADR-0018); boundary-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub widget: Option<String>,
    /// Boundary-only (ADR-0038 §3): the logical channel a signal pipe binds — the input channel
    /// an input pipe reads, or the master channel an output pipe feeds, when the instrument is
    /// played at top level. Omitted for unbound pipes and operator ports.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<usize>,
}

fn port_kind(ty: &PortType) -> &'static str {
    match ty {
        // The glossary's two numeric forms (ADR-0031): a held `f32` is a Value, a dense
        // `f32_buffer` is a Signal. Wire-compatible one way only — a Value source materializes
        // into a Signal input; a Signal into a Value input is a hard plan error.
        PortType::F32 => "value",
        PortType::F32Buffer => "signal",
        PortType::Vocab { name: "Note", .. } => "message",
        PortType::Vocab {
            name: "Harmony", ..
        } => "harmony",
        PortType::Vocab {
            enum_meta: Some(_), ..
        } => "enum",
        PortType::Vocab { .. } => "vocab",
        PortType::I32 { .. } => "int",
        PortType::Str => "string",
        // The type-agnostic pass-through (issue #141) — any Arg, the `osc_out` sink's input.
        PortType::Arg => "arg",
    }
}

/// Widen an `f32` to `f64` without exposing binary-fraction noise: round-trip through the `f32`'s
/// own shortest decimal so `0.2_f32` serializes as `0.2`, not `0.20000000298…` (the naive `as f64`).
fn widen(v: f32) -> f64 {
    v.to_string().parse().unwrap_or(v as f64)
}

fn curve(c: Curve) -> &'static str {
    match c {
        Curve::Linear => "linear",
        Curve::Exponential => "exponential",
    }
}

impl PortInfo {
    /// Flatten one [`Port`] into its agent-facing view, reading whatever metadata its type carries:
    /// the scalar [`F32Meta`](reuben_core::descriptor::F32Meta) on `Port::meta`, or the integer
    /// range / enum variants embedded in the [`PortType`]. `constant` marks a plan-time
    /// [`Constant`](Descriptor::constants) (ADR-0035) so the consumer routes it to `config`.
    fn from_port(p: &Port, constant: bool) -> Self {
        let mut info = PortInfo {
            name: p.name.to_string(),
            kind: port_kind(&p.ty).to_string(),
            constant,
            default: None,
            min: None,
            max: None,
            unit: String::new(),
            curve: None,
            variants: Vec::new(),
            label: None,
            widget: None,
            channel: None,
        };
        // Scalar control (ADR-0030): a materialized `f32` input owns its range/curve/default in `meta`.
        if let Some(m) = &p.meta {
            info.default = Some(serde_json::json!(widen(m.default)));
            info.min = Some(widen(m.min));
            info.max = Some(widen(m.max));
            info.unit = m.unit.to_string();
            info.curve = Some(curve(m.curve).to_string());
        }
        // Type-embedded metadata: an integer's range (ADR-0035) or an enum's named choices (ADR-0030).
        match &p.ty {
            PortType::I32 { meta: Some(m) } => {
                info.default = Some(serde_json::json!(m.default));
                info.min = Some(m.min as f64);
                info.max = Some(m.max as f64);
            }
            PortType::Vocab {
                enum_meta: Some(e), ..
            } => {
                info.default = Some(serde_json::json!(e.default_symbol()));
                info.variants = e.variants.iter().map(|v| v.to_string()).collect();
            }
            _ => {}
        }
        info
    }
}

impl OperatorInfo {
    fn from_descriptor(d: &Descriptor) -> Self {
        // One input surface: runtime inputs, then plan-time `Constant` ports (ADR-0035) flagged
        // `constant`. A port's `kind` + metadata already distinguish scalar / integer / enum, so
        // there is no separate `params`/`enums`/`constants` split to keep in sync.
        let mut inputs: Vec<PortInfo> = d
            .inputs
            .iter()
            .map(|p| PortInfo::from_port(p, false))
            .collect();
        inputs.extend(d.constants.iter().map(|p| PortInfo::from_port(p, true)));
        OperatorInfo {
            type_name: d.type_name.to_string(),
            inputs,
            outputs: d
                .outputs
                .iter()
                .map(|p| PortInfo::from_port(p, false))
                .collect(),
            resources: d.resources.iter().map(|r| r.name.to_string()).collect(),
        }
    }
}

/// Describe the operator set: `which = None` lists every registered operator (deterministic
/// order), `Some(name)` returns just that one — erroring if the registry has no such type.
pub fn describe(registry: &Registry, which: Option<&str>) -> Result<Vec<OperatorInfo>, String> {
    match which {
        None => Ok(registry
            .entries()
            .map(|e| OperatorInfo::from_descriptor(&e.descriptor))
            .collect()),
        Some(name) => match registry.get(name) {
            Some(e) => Ok(vec![OperatorInfo::from_descriptor(&e.descriptor)]),
            None => Err(format!("unknown operator type {name:?}")),
        },
    }
}

/// A nested instrument's synthesized boundary (ADR-0034 §4 / ADR-0038 §2), described **as if it
/// were an Operator**: one [`PortInfo`] per `interface` name — an input pipe from its own declared
/// type/range/default, an output pipe inheriting type and metadata from the internal port feeding
/// it plus optional min/max range overrides (a subset of that port's range). Both carry the entry's
/// presentational fields (label/unit/widget). This is the introspection view of the boundary face a
/// `subpatch` node presents (P6, #121).
#[derive(Debug, Serialize)]
pub struct PatchBoundary {
    /// The document's `instrument` name.
    pub instrument: String,
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
    /// Declared boundary ports whose internal target went dark this load (an unavailable
    /// nested child, ADR-0016/0034) — real ports the description can't type.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dark_inputs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dark_outputs: Vec<String>,
    /// Non-fatal load warnings (unresolved resources etc.), advisory as in `validate`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

impl PatchBoundary {
    /// No boundary port of any kind — typed or dark, either direction. The instrument nests but
    /// exposes nothing to wire. Kept here (not at a call site) so a fifth port collection
    /// can't be forgotten by one of the views.
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
            && self.outputs.is_empty()
            && self.dark_inputs.is_empty()
            && self.dark_outputs.is_empty()
    }
}

impl PortInfo {
    /// The CLI view of one core [`BoundaryPortDesc`] (ADR-0034 §4 / ADR-0038 §2): core's
    /// [`describe_boundary`] already resolved each pipe (an input pipe's declared metadata, an
    /// output pipe's inherited-then-decorated metadata); this only maps its typed fields onto
    /// the flat agent-facing shape shared with operator ports.
    fn from_boundary(b: BoundaryPortDesc) -> Self {
        PortInfo {
            name: b.name,
            kind: port_kind(&b.ty).to_string(),
            constant: false,
            default: b.default.map(|v| match v {
                DocValue::Number(n) => serde_json::json!(n),
                DocValue::Symbol(s) => serde_json::json!(s),
            }),
            min: b.min,
            max: b.max,
            unit: b.unit,
            curve: b.curve.map(|c| curve(c).to_string()),
            variants: b.variants,
            label: b.label,
            widget: b.widget,
            channel: b.channel,
        }
    }
}

/// Describe an instrument document's boundary the way a host instrument will see it (ADR-0034 §4 /
/// ADR-0038 §2): load it through the real engine path (parsed once), let core's
/// [`describe_boundary`] resolve each pipe (an input pipe from its own declared type/range, an
/// output pipe inheriting from the port feeding it plus optional min/max range overrides — both
/// decorated with the entry's presentational fields), and present each port as an operator-style
/// [`PortInfo`]. `kind` is the pipe's `Arg`
/// type — declared on an input pipe, the feeding port's on an output. An instrument with no
/// `interface` yields empty port lists (it nests, but exposes nothing to wire).
pub fn describe_patch(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> Result<PatchBoundary, String> {
    // Parse WITH the resolver (the same way `play` does): a v1 entry re-exporting a nested
    // child's boundary port migrates to the child's real pipe type; the resolver-less
    // `from_json` would fall back to `"f32"` and this description would diverge from what the
    // engine actually loads.
    let doc =
        InstrumentDoc::from_json_with(json, registry, Some(resolver)).map_err(|e| e.to_string())?;
    let loaded = load_instrument_doc(&doc, registry, resolver).map_err(|e| e.to_string())?;
    let b = describe_boundary(&doc, &loaded);

    Ok(PatchBoundary {
        instrument: doc.instrument,
        inputs: b.inputs.into_iter().map(PortInfo::from_boundary).collect(),
        outputs: b.outputs.into_iter().map(PortInfo::from_boundary).collect(),
        dark_inputs: b.dark_inputs,
        dark_outputs: b.dark_outputs,
        warnings: loaded.warnings.iter().map(|w| w.to_string()).collect(),
    })
}

/// One validation problem, with the offending node/port when the loader localized it.
#[derive(Debug, Serialize)]
pub struct Diag {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<String>,
    pub message: String,
}

impl Diag {
    /// Carry the loader's human message verbatim, but pull the node/port the loader already
    /// localized into structured fields so an agent can jump straight to the offending node.
    fn from_load(e: &LoadError) -> Self {
        let (node, port) = match e {
            LoadError::UnknownType { address, .. } => (Some(address.clone()), None),
            LoadError::DuplicateAddress(a) | LoadError::UnknownNode(a) => (Some(a.clone()), None),
            LoadError::UnknownPort { node, port } => (Some(node.clone()), Some(port.clone())),
            LoadError::UnknownInput { node, input } => (Some(node.clone()), Some(input.clone())),
            LoadError::BadInputValue { node, input, .. } => {
                (Some(node.clone()), Some(input.clone()))
            }
            LoadError::UnknownConfig { node, .. }
            | LoadError::ConstantInInputs { node, .. }
            | LoadError::AmbiguousWire { node, .. }
            | LoadError::UnknownResource { node, .. } => (Some(node.clone()), None),
            // A boundary-named problem: the offending "node" is the interface entry itself.
            LoadError::InterfaceOverride { name, .. } | LoadError::InterfacePipe { name, .. } => {
                (None, Some(name.clone()))
            }
            LoadError::TypeMismatch { .. }
            | LoadError::Json(_)
            | LoadError::CyclicResource { .. }
            | LoadError::UnsupportedVersion { .. }
            | LoadError::AnonymousOutputs => (None, None),
        };
        Diag {
            node,
            port,
            message: e.to_string(),
        }
    }
}

/// Outcome of validating an instrument: loadable + cycle-free means `ok`. Resource problems
/// are advisory `warnings` (ADR-0016) and do not flip `ok`.
#[derive(Debug, Serialize)]
pub struct ValidateReport {
    pub ok: bool,
    pub errors: Vec<Diag>,
    pub warnings: Vec<String>,
}

/// Validate an instrument the same way the engine would build it — full load (structural +
/// wiring + kind-checking) plus a `Plan::instantiate` to catch cycles — but with a synthetic
/// [`AudioConfig`], so no audio device is opened and nothing renders.
pub fn validate(
    json: &str,
    registry: &Registry,
    resolver: &dyn ResourceResolver,
) -> ValidateReport {
    let loaded = match load_instrument(json, registry, resolver) {
        Ok(l) => l,
        Err(e) => {
            return ValidateReport {
                ok: false,
                errors: vec![Diag::from_load(&e)],
                warnings: Vec::new(),
            }
        }
    };

    let warnings = loaded.warnings.iter().map(|w| w.to_string()).collect();

    match Plan::instantiate(loaded.graph, AudioConfig::default()) {
        Ok(_) => ValidateReport {
            ok: true,
            errors: Vec::new(),
            warnings,
        },
        Err(reuben_core::plan::PlanError::Cycle) => ValidateReport {
            ok: false,
            errors: vec![Diag {
                node: None,
                port: None,
                message: "graph has a cycle (no valid execution order)".to_string(),
            }],
            warnings,
        },
        Err(reuben_core::plan::PlanError::FormMismatch { src, dst, reason }) => ValidateReport {
            ok: false,
            errors: vec![Diag {
                node: None,
                port: None,
                message: format!("wire {src} → {dst}: {reason}"),
            }],
            warnings,
        },
    }
}
