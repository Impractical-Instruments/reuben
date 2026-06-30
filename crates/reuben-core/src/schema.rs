//! Schema generation — the JSON Schema for instrument documents, derived from the
//! operator descriptors (ADR-0004).
//!
//! The schema is one source of truth: file validation, editor autocomplete, and AI
//! grounding all read it. Because it is generated from the [`Registry`], it cannot drift
//! from the operators (a committed copy is checked for staleness by a test + regenerated
//! by `cargo run -p reuben-core --example gen_schema`).
//!
//! Per-operator validation is emitted as `if`/`then` branches keyed on the node `type`, so each
//! node's `inputs` (literals or wire-refs) and `config` (constants) are checked against the right
//! operator's ports/ranges (ADR-0028).

use serde_json::{json, Map, Value};

use crate::descriptor::PortType;
use crate::registry::Registry;

/// A wire-ref schema (`{ "from": "/node.port" }`) — one input value form (ADR-0028).
fn wire_ref() -> Value {
    json!({
        "type": "object",
        "required": ["from"],
        "additionalProperties": false,
        "properties": { "from": { "type": "string" } },
        "description": "Wire-ref to a source output: \"/node.port\", or \"/node\" when it has one output."
    })
}

/// Build the JSON Schema (draft 2020-12) for instrument documents valid against `registry`.
pub fn generate(registry: &Registry) -> Value {
    let type_names: Vec<Value> = registry.type_names().map(|n| json!(n)).collect();

    // One `if type == X then { inputs, config }` branch per operator.
    let mut branches: Vec<Value> = Vec::new();
    for entry in registry.entries() {
        let d = &entry.descriptor;

        // `inputs`: one property per input port — each accepts a wire-ref, plus the literal forms
        // its shape allows (a `Float` number, an `Enum` symbol/index) — and per non-constant param,
        // which is settable only as a number literal.
        let mut in_props = Map::new();
        for port in &d.inputs {
            let mut forms: Vec<Value> = Vec::new();
            if let Some(e) = port.enum_meta() {
                forms.push(json!({ "type": "string", "enum": e.variants }));
                forms.push(
                    json!({ "type": "integer", "minimum": 0, "maximum": e.variants.len() - 1 }),
                );
            }
            if let Some(m) = &port.meta {
                forms.push(json!({
                    "type": "number",
                    "minimum": m.min as f64,
                    "maximum": m.max as f64,
                    "default": m.default as f64,
                    "description": format!("unit: {}, curve: {:?}", m.unit, m.curve),
                }));
            }
            forms.push(wire_ref());
            in_props.insert(port.name.to_string(), json!({ "oneOf": forms }));
        }

        // `config`: the operator's plan-time `Constant` ports (ADR-0035; today only a voicer's
        // `voices`). An `i32` constant emits an integer range.
        let mut cfg_props = Map::new();
        for c in &d.constants {
            if let PortType::I32 { meta: Some(m) } = &c.ty {
                cfg_props.insert(
                    c.name.to_string(),
                    json!({
                        "type": "integer",
                        "minimum": m.min,
                        "maximum": m.max,
                        "default": m.default,
                        "description": "instantiate-time constant (changing it rebuilds the graph)"
                    }),
                );
            }
        }

        branches.push(json!({
            "if": { "properties": { "type": { "const": d.type_name } }, "required": ["type"] },
            "then": {
                "properties": {
                    "inputs": {
                        "type": "object",
                        "properties": Value::Object(in_props),
                        "additionalProperties": false
                    },
                    "config": {
                        "type": "object",
                        "properties": Value::Object(cfg_props),
                        "additionalProperties": false
                    }
                }
            }
        }));
    }

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "reuben instrument",
        "description": "A reuben instrument: operator nodes (each with an `inputs` map and `config` block) and master outputs (ADR-0028).",
        "type": "object",
        "required": ["instrument", "nodes"],
        "additionalProperties": false,
        "properties": {
            "instrument": { "type": "string" },
            "doc": { "type": "string" },
            "resources": {
                "type": "object",
                "description": "Logical resource id -> source (a file path today). Referenced by a node's `sample` field (ADR-0016).",
                "additionalProperties": { "type": "string" }
            },
            "interface": {
                "type": "object",
                "additionalProperties": false,
                "description": "Engine-honored I/O boundary (ADR-0032): external name -> internal \"/node.port\" wire-ref. `inputs` names map to internal input ports, `outputs` names to output ports (sole-output sugar \"/node\" allowed). A voice patch declares this so its host voicer binds and type-checks it. Distinct from a node's `control` (ADR-0018), which is engine-ignored.",
                "properties": {
                    "inputs": { "type": "object", "additionalProperties": { "type": "string" } },
                    "outputs": { "type": "object", "additionalProperties": { "type": "string" } }
                }
            },
            "nodes": { "type": "array", "items": { "$ref": "#/$defs/node" } },
            "outputs": { "type": "array", "items": { "$ref": "#/$defs/portRef" } }
        },
        "$defs": {
            "portRef": {
                "type": "object",
                "required": ["node", "port"],
                "additionalProperties": false,
                "properties": {
                    "node": { "type": "string" },
                    "port": { "type": "string" },
                    "channel": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Logical master channel an `outputs` tap feeds (ADR-0026): 0 = left, 1 = right, etc. Omitted → broadcast to every channel."
                    }
                }
            },
            "node": {
                "type": "object",
                "required": ["type", "address"],
                "additionalProperties": false,
                "properties": {
                    "type": { "enum": type_names },
                    "address": { "type": "string" },
                    "doc": { "type": "string" },
                    "inputs": { "type": "object" },
                    "config": { "type": "object" },
                    "sample": {
                        "type": "string",
                        "description": "Resource id into the document's `resources` table; only valid on a `sample` node (ADR-0016)."
                    },
                    "control": {
                        "description": "Public-control metadata for a generated control surface (ADR-0018). One control spec, or an array of them for a multi-param node (e.g. a sequencer's steps).",
                        "oneOf": [
                            { "$ref": "#/$defs/controlSpec" },
                            { "type": "array", "items": { "$ref": "#/$defs/controlSpec" } }
                        ]
                    }
                },
                "allOf": branches
            },
            "controlSpec": {
                "type": "object",
                "description": "One player-facing control (ADR-0018); the engine ignores it. `label` is required. With no `param`, the widget binds to the node address (a `map` Good Button, range from its `in_min`/`in_max`); with a `param`, it binds to `/<node>/<param>` (range/`unit`/`default` from the param's metadata). `widget: \"note-toggle\"` emits a toggle that plays a fixed `note` (default 60) through a message `port` (default `notes`), e.g. a voicer's `/voicer/notes [note, gate]`. `widget: \"chord-button\"` (ADR-0022) emits a toggle that sends a fixed scale `degree` (default 0) through a message `port` (default `set`) as `[degree, gate]`, e.g. a chord op's `/chord/set [degree, gate]`. Any field may be overridden here.",
                "required": ["label"],
                "additionalProperties": false,
                "properties": {
                    "label": { "type": "string" },
                    "param": { "type": "string" },
                    "unit": { "type": "string" },
                    "widget": { "type": "string" },
                    "min": { "type": "number" },
                    "max": { "type": "number" },
                    "default": { "type": "number" },
                    "port": { "type": "string" },
                    "note": { "type": "number" },
                    "degree": { "type": "number" }
                }
            }
        }
    })
}

/// The schema as pretty JSON with a trailing newline (the committed on-disk form).
pub fn generate_pretty(registry: &Registry) -> String {
    let mut s = serde_json::to_string_pretty(&generate(registry)).expect("schema serializes");
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_every_operator_type() {
        let schema = generate(&Registry::builtin());
        let types = schema["$defs"]["node"]["properties"]["type"]["enum"]
            .as_array()
            .expect("type enum");
        let names: Vec<&str> = types.iter().filter_map(|v| v.as_str()).collect();
        for expected in ["oscillator", "envelope", "filter", "voicer", "output"] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
    }

    #[test]
    fn emits_input_ranges_per_type() {
        let reg = Registry::builtin();
        let schema = generate(&reg);
        // `cutoff` is a materialized `Float` input (ADR-0028): its `inputs` schema is a `oneOf` of
        // the number form (with the range) and a wire-ref.
        let binding = reg.get("filter").unwrap();
        let (_, filter_cutoff) = binding.descriptor.materialized_input("cutoff").unwrap();

        // Find the if/then branch for "filter" and check its cutoff number-form bounds.
        let branches = schema["$defs"]["node"]["allOf"].as_array().unwrap();
        let filter_branch = branches
            .iter()
            .find(|b| b["if"]["properties"]["type"]["const"] == json!("filter"))
            .expect("filter branch");
        let cutoff = &filter_branch["then"]["properties"]["inputs"]["properties"]["cutoff"];
        let number_form = cutoff["oneOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["type"] == json!("number"))
            .expect("cutoff number form");
        assert_eq!(number_form["minimum"], json!(filter_cutoff.min as f64));
        assert_eq!(number_form["maximum"], json!(filter_cutoff.max as f64));
    }

    #[test]
    fn emits_voices_constant_in_config() {
        let schema = generate(&Registry::builtin());
        let branches = schema["$defs"]["node"]["allOf"].as_array().unwrap();
        let voicer = branches
            .iter()
            .find(|b| b["if"]["properties"]["type"]["const"] == json!("voicer"))
            .expect("voicer branch");
        let voices = &voicer["then"]["properties"]["config"]["properties"]["voices"];
        assert_eq!(voices["type"], json!("integer"));
    }

    #[test]
    fn is_deterministic() {
        assert_eq!(
            generate_pretty(&Registry::builtin()),
            generate_pretty(&Registry::builtin())
        );
    }
}
