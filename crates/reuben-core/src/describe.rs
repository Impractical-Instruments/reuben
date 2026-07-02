//! Boundary introspection (ADR-0034 §4, P6): describe a loaded instrument's `interface` as the
//! operator-style port list a host wires against — type and metadata **inherited** from the
//! inner port each entry names, the entry's presentational overrides applied, the effective
//! default resolved. This is the core-side construction ADR-0034 asks the synthesized face to
//! feed ("feed introspection/schema/docs"): every host — the CLI's `reuben describe`, a wasm or
//! embedded embedder — reads the same inherit+override merge instead of re-implementing it.

use crate::descriptor::{Curve, PortType};
use crate::format::{doc_value, widen_f32, DocValue, InstrumentDoc, Loaded};
use crate::plan::{port_kind, PortKind};

/// One boundary port as a host sees it (ADR-0034 §4): the inner port's type (inherited verbatim,
/// never overridable) and metadata, decorated by the `interface` entry's presentational
/// overrides. Values are owned/document-facing (`f64`, `String`) — a description, not a
/// descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryPortDesc {
    /// The external boundary name — the wiring handle a host uses.
    pub name: String,
    /// The inner port's [`PortType`], inherited verbatim (§4: an override decorates
    /// presentation, never what type flows).
    pub ty: PortType,
    /// Effective unwired value: the child's own literal (a value-override on the inner node)
    /// beats the descriptor default. `None` for a port with no settable default.
    pub default: Option<DocValue>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub unit: String,
    /// Sweep curve for a swept scalar; `None` for non-scalar ports.
    pub curve: Option<Curve>,
    /// The ordered enum choices (ADR-0030); empty for non-enum ports.
    pub variants: Vec<String>,
    /// Display-name override from the entry (§4); `None` when not overridden.
    pub label: Option<String>,
    /// Widget hint override for a generated control surface (ADR-0018).
    pub widget: Option<String>,
    /// An input whose inner **Signal** port the child already drives internally: still a real
    /// boundary port, but a host wire onto it is the fatal
    /// [`BoundaryInputDriven`](crate::format::LoadError::BoundaryInputDriven) — surfaced here so
    /// introspection states the build contract instead of the host discovering it at build.
    pub driven: bool,
}

/// A loaded instrument's boundary (ADR-0034 §4), described as if it were an operator: one entry
/// per `interface` name. An instrument with no `interface` yields empty lists (it nests, but exposes
/// nothing to wire).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoundaryDesc {
    pub inputs: Vec<BoundaryPortDesc>,
    pub outputs: Vec<BoundaryPortDesc>,
    /// Declared boundary ports whose internal target went dark this load (an unavailable nested
    /// child, ADR-0016/0034) — real ports the description can't type.
    pub dark_inputs: Vec<String>,
    pub dark_outputs: Vec<String>,
}

/// Describe `loaded`'s boundary the way a host instrument will see it. `doc` must be the document
/// `loaded` was built from — it carries the `interface` entries' presentational overrides, which
/// the built [`Graph`](crate::graph::Graph) deliberately does not (they are document-level, like
/// `control`). Overrides are load-validated ([ADR-0034 §4's override
/// law](crate::format::LoadError::InterfaceOverride)), so every range here is a subset of what
/// the engine enforces.
pub fn describe_boundary(doc: &InstrumentDoc, loaded: &Loaded) -> BoundaryDesc {
    let g = &loaded.graph;
    let port_desc = |name: &String, key: crate::graph::NodeKey, idx: usize, output: bool| {
        let node = &g.nodes[key];
        let d = &node.descriptor;
        let p = if output {
            &d.outputs[idx]
        } else {
            &d.inputs[idx]
        };
        let mut desc = BoundaryPortDesc {
            name: name.clone(),
            ty: p.ty.clone(),
            default: None,
            min: None,
            max: None,
            unit: String::new(),
            curve: None,
            variants: Vec::new(),
            label: None,
            widget: None,
            driven: false,
        };
        // Inherit (ADR-0030/0035): a swept scalar's F32Meta, an integer's range, an enum's
        // named choices — the same metadata an operator port advertises.
        if let Some(m) = &p.meta {
            desc.default = Some(DocValue::Number(widen_f32(m.default)));
            desc.min = Some(widen_f32(m.min));
            desc.max = Some(widen_f32(m.max));
            desc.unit = m.unit.to_string();
            desc.curve = Some(m.curve);
        }
        match &p.ty {
            PortType::I32 { meta: Some(m) } => {
                desc.default = Some(DocValue::Number(m.default as f64));
                desc.min = Some(m.min as f64);
                desc.max = Some(m.max as f64);
            }
            PortType::Vocab {
                enum_meta: Some(e), ..
            } => {
                desc.default = Some(DocValue::Symbol(e.default_symbol().to_string()));
                desc.variants = e.variants.iter().map(|v| v.to_string()).collect();
            }
            _ => {}
        }
        if !output {
            // The effective unwired value is what a host actually gets: the child's own literal
            // (`"mix": 0.35`, a value-override on the inner node) beats the descriptor default.
            if let Some((_, arg)) = node.value_overrides.iter().find(|(port, _)| *port == idx) {
                desc.default = Some(doc_value(p, arg));
            }
            // An inner Signal port the child already drives: wiring the boundary name is fatal
            // (LoadError::BoundaryInputDriven) — say so here, not first at host build.
            desc.driven = port_kind(p) == PortKind::Signal
                && g.connections
                    .iter()
                    .any(|c| c.dst == key && c.dst_port == idx);
        }
        // Decorate with the entry's presentational overrides (§4) — kind/variants stay.
        let entries = doc
            .interface
            .as_ref()
            .map(|i| if output { &i.outputs } else { &i.inputs });
        if let Some(m) = entries.and_then(|e| e.get(name)).and_then(|e| e.meta()) {
            if let Some(l) = &m.label {
                desc.label = Some(l.clone());
            }
            if let Some(u) = &m.unit {
                desc.unit = u.clone();
            }
            if let Some(w) = &m.widget {
                desc.widget = Some(w.clone());
            }
            if let Some(min) = m.min {
                desc.min = Some(min);
            }
            if let Some(max) = m.max {
                desc.max = Some(max);
            }
        }
        desc
    };

    BoundaryDesc {
        inputs: g
            .interface
            .inputs
            .iter()
            .map(|(name, (k, i))| port_desc(name, *k, *i, false))
            .collect(),
        outputs: g
            .interface
            .outputs
            .iter()
            .map(|(name, (k, i))| port_desc(name, *k, *i, true))
            .collect(),
        dark_inputs: g.interface.dark_inputs.iter().cloned().collect(),
        dark_outputs: g.interface.dark_outputs.iter().cloned().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::load_instrument_doc;
    use crate::registry::Registry;
    use crate::resources::{ResolveError, ResourceResolver, SampleBuffer};

    /// These fixtures reference no samples/patches, so every resolve is a miss.
    struct NoResources;
    impl ResourceResolver for NoResources {
        fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(source.to_string()))
        }
    }

    fn boundary(json: &str) -> BoundaryDesc {
        let doc = InstrumentDoc::from_json(json).expect("parse");
        let loaded = load_instrument_doc(&doc, &Registry::builtin(), &NoResources).expect("load");
        describe_boundary(&doc, &loaded)
    }

    #[test]
    fn boundary_port_inherits_then_applies_overrides() {
        // `tone` inherits /filter.cutoff's type/curve, the child literal beats the descriptor
        // default (1000), and the entry's label + narrowed range decorate the result.
        let b = boundary(
            r#"{"instrument":"t","interface":{
                "inputs":{"tone":{"target":"/filter.cutoff","label":"Tone","min":200,"max":8000}},
                "outputs":{"out":"/filter.audio"}},
            "nodes":[{"type":"filter","address":"/filter","inputs":{"cutoff":4000.0}}]}"#,
        );
        let tone = &b.inputs[0];
        assert_eq!(tone.name, "tone");
        assert_eq!(
            tone.ty,
            PortType::F32Buffer,
            "type inherited, never overridden"
        );
        assert_eq!(
            tone.default,
            Some(DocValue::Number(4000.0)),
            "child literal wins"
        );
        assert_eq!((tone.min, tone.max), (Some(200.0), Some(8000.0)));
        assert_eq!(tone.unit, "Hz", "un-overridden unit stays inherited");
        assert_eq!(tone.curve, Some(Curve::Exponential));
        assert_eq!(tone.label.as_deref(), Some("Tone"));
        assert!(!tone.driven, "unwired inner port is host-wireable");
        assert_eq!(b.outputs[0].name, "out");
    }

    #[test]
    fn internally_driven_signal_input_is_flagged() {
        // The child drives /filter.audio itself; the exposed boundary name is a real port, but a
        // host wire onto it is fatal (BoundaryInputDriven) — the description must say so.
        let b = boundary(
            r#"{"instrument":"t","interface":{
                "inputs":{"in":"/filter.audio","tone":"/filter.cutoff"}},
            "nodes":[
                {"type":"oscillator","address":"/osc"},
                {"type":"filter","address":"/filter","inputs":{"audio":{"from":"/osc.audio"}}}]}"#,
        );
        let by_name = |n: &str| b.inputs.iter().find(|p| p.name == n).expect(n).driven;
        assert!(by_name("in"), "internally driven Signal input flags driven");
        assert!(!by_name("tone"), "unwired input stays wireable");
    }
}
