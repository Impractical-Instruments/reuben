//! Schema generation — the JSON Schema for instrument documents, derived from the
//! operator descriptors (ADR-0004).
//!
//! The schema is one source of truth: file validation, editor autocomplete, and AI
//! grounding all read it. Because it is generated from the [`Registry`], it cannot drift
//! from the operators (a committed copy is checked for staleness by a test + regenerated
//! by `cargo run -p reuben-core --example gen_schema`).
//!
//! Per-operator param validation is emitted as `if`/`then` branches keyed on the node
//! `type`, so `params` are checked against the right operator's ranges.

use serde_json::{json, Map, Value};

use crate::registry::Registry;

/// Build the JSON Schema (draft 2020-12) for instrument documents valid against `registry`.
pub fn generate(registry: &Registry) -> Value {
    let type_names: Vec<Value> = registry.type_names().map(|n| json!(n)).collect();

    // One `if type == X then params: {ranges}` branch per operator.
    let mut branches: Vec<Value> = Vec::new();
    for entry in registry.entries() {
        let d = &entry.descriptor;
        let mut props = Map::new();
        for p in &d.params {
            props.insert(
                p.name.to_string(),
                json!({
                    "type": "number",
                    "minimum": p.min as f64,
                    "maximum": p.max as f64,
                    "default": p.default as f64,
                    "description": format!("unit: {}, curve: {:?}", p.unit, p.curve),
                }),
            );
        }
        // Materialized Float inputs (ADR-0028) are settable literals too — the old
        // "signal port + same-named unwired-default param" is now one input, addressed by the
        // same name in the `params` map (the loader bridges it). Emit them with identical
        // metadata so an author can still set e.g. `oscillator` `freq`/`waveform`.
        for p in &d.inputs {
            if let Some(m) = &p.meta {
                props.insert(
                    p.name.to_string(),
                    json!({
                        "type": "number",
                        "minimum": m.min as f64,
                        "maximum": m.max as f64,
                        "default": m.default as f64,
                        "description": format!("unit: {}, curve: {:?}", m.unit, m.curve),
                    }),
                );
            }
        }
        branches.push(json!({
            "if": { "properties": { "type": { "const": d.type_name } }, "required": ["type"] },
            "then": {
                "properties": {
                    "params": {
                        "type": "object",
                        "properties": Value::Object(props),
                        "additionalProperties": false
                    }
                }
            }
        }));
    }

    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "reuben instrument",
        "description": "A reuben instrument: operator nodes, port connections, and master outputs.",
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
            "nodes": { "type": "array", "items": { "$ref": "#/$defs/node" } },
            "connections": { "type": "array", "items": { "$ref": "#/$defs/connection" } },
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
                        "description": "Logical master channel an `outputs` tap feeds (ADR-0026): 0 = left, 1 = right, etc. Omitted → broadcast to every channel. Ignored on a connection endpoint."
                    }
                }
            },
            "connection": {
                "type": "object",
                "required": ["from", "to"],
                "additionalProperties": false,
                "properties": {
                    "from": { "$ref": "#/$defs/portRef" },
                    "to": { "$ref": "#/$defs/portRef" }
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
                    "params": { "type": "object" },
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
                "description": "One player-facing control (ADR-0018); the engine ignores it. `label` is required. With no `param`, the widget binds to the node address (a `map` Good Button, range from its `in_min`/`in_max`); with a `param`, it binds to `/<node>/<param>` (range/`unit`/`default` from the param's metadata). `widget: \"note-toggle\"` emits a toggle that plays a fixed `note` (default 60) through a message `port` (default `note`), e.g. a voicer's `/voicer/note [note, gate]`. `widget: \"chord-button\"` (ADR-0022) emits a toggle that sends a fixed scale `degree` (default 0) through a message `port` (default `set`) as `[degree, gate]`, e.g. a chord op's `/chord/set [degree, gate]`. Any field may be overridden here.",
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
    fn emits_param_ranges_per_type() {
        let reg = Registry::builtin();
        let schema = generate(&reg);
        let filter_cutoff = reg
            .get("filter")
            .unwrap()
            .descriptor
            .params
            .iter()
            .find(|p| p.name == "cutoff")
            .unwrap();

        // Find the if/then branch for "filter" and check its cutoff bounds.
        let branches = schema["$defs"]["node"]["allOf"].as_array().unwrap();
        let filter_branch = branches
            .iter()
            .find(|b| b["if"]["properties"]["type"]["const"] == json!("filter"))
            .expect("filter branch");
        let cutoff = &filter_branch["then"]["properties"]["params"]["properties"]["cutoff"];
        assert_eq!(cutoff["minimum"], json!(filter_cutoff.min as f64));
        assert_eq!(cutoff["maximum"], json!(filter_cutoff.max as f64));
    }

    #[test]
    fn is_deterministic() {
        assert_eq!(
            generate_pretty(&Registry::builtin()),
            generate_pretty(&Registry::builtin())
        );
    }
}
