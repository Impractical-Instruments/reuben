//! CLI introspection for the Patcher skill (ADR-0020): `describe` the operator set and
//! `validate` an instrument without touching audio hardware.
//!
//! These back the `reuben describe` / `reuben validate` subcommands but are pure functions
//! over [`Registry`] + JSON so they test through real load/plan code paths, not a process.

use reuben_core::descriptor::{Curve, Descriptor, PortKind};
use reuben_core::format::LoadError;
use reuben_core::plan::Plan;
use reuben_core::resources::ResourceResolver;
use reuben_core::{load_instrument, AudioConfig, Registry};

use serde::Serialize;

/// One operator's self-description, flattened from its [`Descriptor`] for agent grounding.
#[derive(Debug, Serialize)]
pub struct OperatorInfo {
    pub type_name: String,
    pub inputs: Vec<PortInfo>,
    pub outputs: Vec<PortInfo>,
    pub params: Vec<ParamInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct PortInfo {
    pub name: String,
    /// `"signal"`, `"message"`, or `"context"`.
    pub kind: String,
}

#[derive(Debug, Serialize)]
pub struct ParamInfo {
    pub name: String,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    #[serde(skip_serializing_if = "str::is_empty")]
    pub unit: String,
    /// `"linear"` or `"exponential"`.
    pub curve: String,
}

fn port_kind(k: PortKind) -> &'static str {
    match k {
        PortKind::Signal => "signal",
        PortKind::Message => "message",
        PortKind::Context => "context",
    }
}

fn curve(c: Curve) -> &'static str {
    match c {
        Curve::Linear => "linear",
        Curve::Exponential => "exponential",
    }
}

impl OperatorInfo {
    fn from_descriptor(d: &Descriptor) -> Self {
        let ports = |ps: &[reuben_core::descriptor::Port]| {
            ps.iter()
                .map(|p| PortInfo {
                    name: p.name.to_string(),
                    kind: port_kind(p.kind).to_string(),
                })
                .collect()
        };
        let mut params: Vec<ParamInfo> = d
            .params
            .iter()
            .map(|p| ParamInfo {
                name: p.name.to_string(),
                default: p.default,
                min: p.min,
                max: p.max,
                unit: p.unit.to_string(),
                curve: curve(p.curve).to_string(),
            })
            .collect();
        // Materialized Float inputs (ADR-0028) are settable literals — the old "signal port +
        // same-named unwired-default param" is now one input, addressed by the same name. Surface
        // their range/default alongside params so `describe` still shows what an author can set.
        for p in &d.inputs {
            if let Some(m) = &p.meta {
                params.push(ParamInfo {
                    name: p.name.to_string(),
                    default: m.default,
                    min: m.min,
                    max: m.max,
                    unit: m.unit.to_string(),
                    curve: curve(m.curve).to_string(),
                });
            }
        }
        OperatorInfo {
            type_name: d.type_name.to_string(),
            inputs: ports(&d.inputs),
            outputs: ports(&d.outputs),
            params,
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
            LoadError::UnknownParam { node, .. } | LoadError::UnknownResource { node, .. } => {
                (Some(node.clone()), None)
            }
            LoadError::PortKindMismatch { .. } | LoadError::Json(_) => (None, None),
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
    }
}
