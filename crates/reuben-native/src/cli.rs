//! CLI introspection for the Patcher skill (ADR-0020): `describe` the operator set and
//! `validate` an instrument without touching audio hardware.
//!
//! These back the `reuben describe` / `reuben validate` subcommands but are pure functions
//! over [`Registry`] + JSON so they test through real load/plan code paths, not a process.

use reuben_core::descriptor::{Curve, Descriptor, Port, PortType};
use reuben_core::format::LoadError;
use reuben_core::plan::Plan;
use reuben_core::resources::ResourceResolver;
use reuben_core::{load_instrument, AudioConfig, Registry};

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
/// — an immutable port set in a patch's `config` block, never wired in `inputs`. Optional metadata
/// appears only where the port's type carries it: `default`/`min`/`max`/`unit`/`curve` for a swept
/// scalar, `default`/`min`/`max` for an integer, `default`/`variants` for an enum.
#[derive(Debug, Serialize)]
pub struct PortInfo {
    pub name: String,
    /// The port's [`PortType`] as a word: `"signal"` (F32/Buffer), `"int"`, `"enum"`, `"message"`
    /// (Note), `"harmony"` (Harmony), `"vocab"`, or `"string"`.
    pub kind: String,
    /// A plan-time `Constant` (ADR-0035): set in the patch `config` block, not wired in `inputs`.
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
}

fn port_kind(ty: &PortType) -> &'static str {
    match ty {
        PortType::F32 | PortType::F32Buffer => "signal",
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
            LoadError::TypeMismatch { .. }
            | LoadError::Json(_)
            | LoadError::CyclicResource { .. } => (None, None),
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
