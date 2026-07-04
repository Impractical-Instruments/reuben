//! Boundary introspection (ADR-0034 §4 / ADR-0038): describe a loaded instrument's `interface`
//! as the operator-style port list a host wires against. An **input pipe** is described from
//! its own declaration (the synthesized pipe descriptor carries the declared type, range, and
//! default; the document entry carries the presentational fields); an **output pipe** inherits
//! type and metadata from the internal port that feeds it, decorated by the entry's
//! presentational overrides. This is the core-side construction every host — the CLI's
//! `reuben describe`, a wasm or embedded embedder — reads instead of re-implementing the merge.

use crate::descriptor::{Curve, PortType};
use crate::format::{doc_value, widen_f32, DocValue, InstrumentDoc, InterfaceEntry, Loaded};

/// One boundary port as a host sees it (ADR-0034 §4 / ADR-0038 §2): an input pipe's **declared**
/// type/range/default, or an output pipe's type and metadata inherited from the internal port
/// feeding it, decorated by the entry's presentational fields. Values are owned/document-facing
/// (`f64`, `String`) — a description, not a descriptor.
#[derive(Debug, Clone, PartialEq)]
pub struct BoundaryPortDesc {
    /// The external boundary name — the wiring handle a host uses.
    pub name: String,
    /// The pipe's `Arg` type: declared on an input pipe (nothing to inherit from — ADR-0038),
    /// inherited from the feeding port on an output pipe.
    pub ty: PortType,
    /// Effective unwired value: an input pipe's seed (a host literal beats the declared
    /// default), an output pipe's inherited default. `None` for a port with no settable default.
    pub default: Option<DocValue>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub unit: String,
    /// Sweep curve for a swept scalar; `None` for non-scalar ports.
    pub curve: Option<Curve>,
    /// The ordered enum choices (ADR-0030); empty for non-enum ports.
    pub variants: Vec<String>,
    /// Display-name from the entry; `None` when not set.
    pub label: Option<String>,
    /// Widget hint for a generated control surface (ADR-0018).
    pub widget: Option<String>,
    /// Logical channel binding (ADR-0038 §3): the input channel a signal input pipe reads, or
    /// the master channel a signal output pipe feeds, when the graph plays at top level.
    pub channel: Option<usize>,
}

/// A loaded instrument's boundary (ADR-0034 §4 / ADR-0038), described as if it were an operator:
/// one entry per `interface` name. An instrument with no `interface` yields empty lists (it
/// nests, but exposes nothing to wire).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BoundaryDesc {
    pub inputs: Vec<BoundaryPortDesc>,
    pub outputs: Vec<BoundaryPortDesc>,
    /// Declared boundary ports whose internal target went dark this load (an unavailable nested
    /// child, ADR-0016/0034) — real ports the description can't type. Always outputs in v2
    /// (an input pipe is self-contained), kept per-direction for the host view.
    pub dark_inputs: Vec<String>,
    pub dark_outputs: Vec<String>,
}

/// Describe `loaded`'s boundary the way a host instrument will see it. `doc` must be the document
/// `loaded` was built from — it carries the entries' presentational fields (label/unit/widget),
/// which the built [`Graph`](crate::graph::Graph) deliberately does not (they are
/// document-level, like `control`). Output overrides are load-validated (the ADR-0034 §4
/// subset law), so every range here is a subset of what the engine enforces; an input pipe's
/// range *is* what the engine enforces (ADR-0038 §2).
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
            channel: None,
        };
        // The port's own metadata: an input pipe's declared range/default (its descriptor was
        // synthesized from the entry), or the feeding port's inherited metadata for an output.
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
            // The effective seed is what an unfed pipe forwards: a host literal (a
            // value-override on the pipe node) beats the declared default.
            if let Some((_, arg)) = node.value_overrides.iter().find(|(port, _)| *port == idx) {
                desc.default = Some(doc_value(p, arg));
            }
        }
        // Decorate with the entry's presentational fields — type/range/variants stay.
        let entries = doc
            .interface
            .as_ref()
            .map(|i| if output { &i.outputs } else { &i.inputs });
        match entries.and_then(|e| e.get(name)) {
            Some(InterfaceEntry::Pipe(p)) => {
                desc.label = p.label.clone();
                desc.widget = p.widget.clone();
                if let Some(u) = &p.unit {
                    desc.unit = u.clone();
                }
            }
            Some(InterfaceEntry::Feed(f)) => {
                desc.label = f.label.clone();
                desc.widget = f.widget.clone();
                if let Some(u) = &f.unit {
                    desc.unit = u.clone();
                }
                if let Some(min) = f.min {
                    desc.min = Some(min);
                }
                if let Some(max) = f.max {
                    desc.max = Some(max);
                }
            }
            _ => {}
        }
        desc.channel = if output {
            g.interface.output_channels.get(name).copied()
        } else {
            g.interface.input_channels.get(name).copied()
        };
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
        let doc = InstrumentDoc::from_json(json, &Registry::builtin()).expect("parse");
        let loaded = load_instrument_doc(&doc, &Registry::builtin(), &NoResources).expect("load");
        describe_boundary(&doc, &loaded)
    }

    #[test]
    fn input_pipe_describes_its_own_declaration() {
        // A native-v2 pipe: the declared type/range/default/curve and presentation come from
        // the entry itself — nothing inherited (ADR-0038 §2).
        let b = boundary(
            r#"{"format_version":2,"instrument":"t","interface":{
                "inputs":{"tone":{"type":"f32_buffer","default":4000,"min":200,"max":8000,
                                  "curve":"exp","unit":"Hz","label":"Tone","widget":"knob"}},
                "outputs":{"out":{"from":"/filter.audio"}}},
            "nodes":[{"type":"filter","address":"/filter",
                      "inputs":{"cutoff":{"from":"/tone"}}}]}"#,
        );
        let tone = &b.inputs[0];
        assert_eq!(tone.name, "tone");
        assert_eq!(tone.ty, PortType::F32Buffer, "declared type");
        assert_eq!(tone.default, Some(DocValue::Number(4000.0)));
        assert_eq!((tone.min, tone.max), (Some(200.0), Some(8000.0)));
        assert_eq!(tone.unit, "Hz", "unit comes from the entry");
        assert_eq!(tone.curve, Some(Curve::Exponential));
        assert_eq!(tone.label.as_deref(), Some("Tone"));
        assert_eq!(tone.widget.as_deref(), Some("knob"));
        assert_eq!(b.outputs[0].name, "out");
        assert_eq!(
            b.outputs[0].ty,
            PortType::F32Buffer,
            "output inherits the feeding port's type"
        );
    }

    #[test]
    fn migrated_v1_boundary_describes_like_the_original() {
        // The v1 fixture from ADR-0034: `tone` targeted /filter.cutoff with overrides. After
        // migration the pipe carries the **inner port's engine range** (v1's min/max were
        // presentational — the engine clamped to the target's own range, and a v2 pipe range
        // is engine-enforced, so the narrowing cannot ride along), the child literal became
        // its default, and the label decorates. What describe publishes is what the engine
        // enforces — the ADR-0034 §4 truthfulness law, now with nothing lost in between.
        let b = boundary(
            r#"{"instrument":"t","interface":{
                "inputs":{"tone":{"target":"/filter.cutoff","label":"Tone","min":200,"max":8000}},
                "outputs":{"out":"/filter.audio"}},
            "nodes":[{"type":"filter","address":"/filter","inputs":{"cutoff":4000.0}}]}"#,
        );
        let tone = &b.inputs[0];
        assert_eq!(tone.ty, PortType::F32Buffer, "type derived from the target");
        assert_eq!(
            tone.default,
            Some(DocValue::Number(4000.0)),
            "child literal became the pipe default"
        );
        assert_eq!(
            (tone.min, tone.max),
            (Some(20.0), Some(20_000.0)),
            "the migrated pipe advertises the engine-enforced (inner-port) range"
        );
        assert_eq!(tone.unit, "Hz", "target port's unit carried over");
        assert_eq!(tone.curve, Some(Curve::Exponential));
        assert_eq!(tone.label.as_deref(), Some("Tone"));
    }

    #[test]
    fn channel_bindings_surface_on_the_description() {
        let b = boundary(
            r#"{"format_version":2,"instrument":"t","interface":{
                "inputs":{"mic":{"type":"f32_buffer","channel":1}},
                "outputs":{"main_l":{"from":"/gain.out","channel":0}}},
            "nodes":[{"type":"mul_f32_signal","address":"/gain",
                      "inputs":{"a":{"from":"/mic"}}}]}"#,
        );
        assert_eq!(b.inputs[0].channel, Some(1));
        assert_eq!(b.outputs[0].channel, Some(0));
    }

    #[test]
    fn channel_plus_default_pipe_describes_both_truthfully() {
        // #190 F1: a `channel` + `default` pipe is *both* a device input and a knob — the
        // engine honors the default as the unfed fallback (and keeps the pipe
        // message-drivable), so describe advertising both is the truth, not a lie.
        let b = boundary(
            r#"{"format_version":2,"instrument":"t","interface":{
                "inputs":{"lvl":{"type":"f32_buffer","channel":0,
                                 "default":0.25,"min":0,"max":1}},
                "outputs":{"main":{"from":"/gain.out"}}},
            "nodes":[{"type":"mul_f32_signal","address":"/gain",
                      "inputs":{"a":{"from":"/lvl"}}}]}"#,
        );
        let lvl = &b.inputs[0];
        assert_eq!(lvl.channel, Some(0), "the device binding surfaces");
        assert_eq!(
            lvl.default,
            Some(DocValue::Number(0.25)),
            "the unfed fallback default surfaces"
        );
        assert_eq!((lvl.min, lvl.max), (Some(0.0), Some(1.0)));
    }
}
