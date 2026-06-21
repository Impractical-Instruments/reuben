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
            "nodes": { "type": "array", "items": { "$ref": "#/$defs/node" } },
            "connections": { "type": "array", "items": { "$ref": "#/$defs/connection" } },
            "outputs": { "type": "array", "items": { "$ref": "#/$defs/portRef" } }
        },
        "$defs": {
            "portRef": {
                "type": "object",
                "required": ["node", "port"],
                "additionalProperties": false,
                "properties": { "node": { "type": "string" }, "port": { "type": "string" } }
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
                    "params": { "type": "object" }
                },
                "allOf": branches
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
