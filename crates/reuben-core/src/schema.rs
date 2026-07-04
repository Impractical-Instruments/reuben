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

        // A nested-instrument node (ADR-0034): its ports are the referenced patch's `interface`
        // names, synthesized at load — unknowable from the static descriptor this schema is
        // generated from. Allow free-form `inputs` in the generic value shapes (wire-ref, number,
        // symbol); names and types are checked by the loader against the synthesized boundary
        // face. `config` stays locked to the (empty) constant set below.
        if d.has_resource("patch") {
            branches.push(json!({
                "if": { "properties": { "type": { "const": d.type_name } }, "required": ["type"] },
                "then": {
                    "properties": {
                        "inputs": {
                            "type": "object",
                            "additionalProperties": { "oneOf": [
                                { "type": "number" },
                                { "type": "string" },
                                wire_ref()
                            ] },
                            "description": "Boundary inputs, named by the referenced patch's `interface` (ADR-0034 §4). Port names and Arg types are validated at load against the resolved patch, not by this schema."
                        },
                        "config": {
                            "type": "object",
                            "properties": {},
                            "additionalProperties": false
                        }
                    }
                }
            }));
            continue;
        }

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
            "format_version": {
                "type": "integer",
                "minimum": 1,
                "description": "Document format version (ADR-0036). Absent means 1; saving writes the current version. A version newer than the engine understands refuses to load."
            },
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
                "description": "Engine-honored I/O boundary as named PIPES (ADR-0038, format v2). Each `inputs` entry declares its Arg type and mints an address in the flat node namespace (`in` -> `/in`) that internal nodes consume with ordinary wire-refs (`{\"from\": \"/in\"}`, fan-out free). Each `outputs` entry is fed from an internal port (`{\"from\": \"/pan.left\", \"channel\": 0}`); signal output pipes are the master taps. A voice patch declares this so its host voicer binds and type-checks it; a nested instrument's `subpatch` ports are synthesized from it. Distinct from a node's `control` (ADR-0018), which is engine-ignored. (The v1 target-pointing entry forms auto-migrate at load and are not valid v2 — this schema describes v2.)",
                "properties": {
                    "inputs": { "type": "object", "additionalProperties": { "$ref": "#/$defs/inputPipe" } },
                    "outputs": { "type": "object", "additionalProperties": { "$ref": "#/$defs/outputPipe" } }
                }
            },
            "nodes": { "type": "array", "items": { "$ref": "#/$defs/node" } },
            "outputs": {
                "type": "array",
                "items": { "$ref": "#/$defs/portRef" },
                "description": "v1-only (ADR-0038 §4): the anonymous master-tap list. Auto-migrated into named `interface.outputs` pipes at load; a v2 document must not carry it."
            }
        },
        "$defs": {
            "inputPipe": {
                "type": "object",
                "required": ["type"],
                "additionalProperties": false,
                "description": "One interface INPUT pipe (ADR-0038 §2): a named source in the flat namespace (`in` -> `/in`; a post-mint collision is a fatal duplicate address). It DECLARES its Arg type — nothing to inherit from — enforced against every consumer by the ordinary wire check. Numeric pipes may declare an engine-enforced default/min/max/curve (an unwired signal pipe materializes `default`; a bare one, silence). A SIGNAL pipe may bind a logical input `channel`, honored only when the graph is played at top level (inert nested/hosted); `channel` on a message pipe is a load error.",
                "properties": {
                    "type": { "type": "string", "description": "The declared Arg type: \"f32_buffer\" (Signal), \"f32\" (held Value), \"note\" (Event), \"harmony\" (held Value), or a shared vocab enum name (e.g. \"FilterMode\")." },
                    "channel": { "type": "integer", "minimum": 0, "description": "Logical input channel binding (signal pipes only; top-level-honored, ADR-0038 §3)." },
                    "default": {
                        "description": "Unwired/seed value: a number for a numeric pipe, the variant symbol for an enum pipe.",
                        "oneOf": [ { "type": "number" }, { "type": "string" } ]
                    },
                    "min": { "type": "number", "description": "Engine-enforced range floor (numeric pipes; defaults to the type-wide bound)." },
                    "max": { "type": "number", "description": "Engine-enforced range ceiling (numeric pipes)." },
                    "curve": { "enum": ["lin", "exp"], "description": "Sweep-curve hint for a numeric pipe; defaults to lin." },
                    "unit": { "type": "string" },
                    "label": { "type": "string" },
                    "widget": { "type": "string" }
                }
            },
            "outputPipe": {
                "type": "object",
                "required": ["from"],
                "additionalProperties": false,
                "description": "One interface OUTPUT pipe (ADR-0038 §2): fed from an internal port by an ordinary wire-ref. `channel` pins the logical master channel a signal pipe feeds (omitted = broadcast, ADR-0026); on a message-typed pipe it is a load error. min/max are presentational overrides obeying the ADR-0034 §4 subset law against the feeding port's range.",
                "properties": {
                    "from": { "type": "string", "description": "Internal \"/node.port\" (or sole-output \"/node\") wire-ref feeding this pipe." },
                    "channel": { "type": "integer", "minimum": 0, "description": "Logical master output channel (signal pipes only); omitted = broadcast." },
                    "label": { "type": "string" },
                    "unit": { "type": "string" },
                    "widget": { "type": "string" },
                    "min": { "type": "number" },
                    "max": { "type": "number" }
                }
            },
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
                    "patch": {
                        "type": "string",
                        "description": "Resource id into the document's `resources` table naming a nested instrument patch; only valid on a `subpatch` node (ADR-0034). At build the referenced patch is loaded and inlined: its nodes are spliced in under this node's address prefix, its `interface` becomes this node's ports, and the node dissolves."
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
    fn subpatch_inputs_are_free_form() {
        // ADR-0034 §4: a subpatch's ports are synthesized from the referenced patch's `interface`
        // at load, so the schema cannot enumerate them — it must accept any input name in the
        // generic value shapes and leave name/type validation to the loader.
        let schema = generate(&Registry::builtin());
        let branches = schema["$defs"]["node"]["allOf"].as_array().unwrap();
        let subpatch = branches
            .iter()
            .find(|b| b["if"]["properties"]["type"]["const"] == json!("subpatch"))
            .expect("subpatch branch");
        let inputs = &subpatch["then"]["properties"]["inputs"];
        assert!(
            inputs
                .get("additionalProperties")
                .is_some_and(|v| v != &json!(false)),
            "subpatch inputs accept interface-named entries: {inputs}"
        );
        // `config` stays locked — a subpatch declares no constants.
        let config = &subpatch["then"]["properties"]["config"];
        assert_eq!(config["additionalProperties"], json!(false));
    }

    #[test]
    fn interface_maps_reference_the_pipe_defs() {
        // ADR-0038 §2: inputs are typed pipes, outputs are fed-from entries — two distinct
        // closed shapes, each with `additionalProperties: false` matching the loader's
        // `deny_unknown_fields`.
        let schema = generate(&Registry::builtin());
        let iface = &schema["properties"]["interface"]["properties"];
        assert_eq!(
            iface["inputs"]["additionalProperties"]["$ref"],
            json!("#/$defs/inputPipe")
        );
        assert_eq!(
            iface["outputs"]["additionalProperties"]["$ref"],
            json!("#/$defs/outputPipe")
        );
        let input = &schema["$defs"]["inputPipe"];
        assert_eq!(input["required"], json!(["type"]));
        assert_eq!(input["additionalProperties"], json!(false));
        let output = &schema["$defs"]["outputPipe"];
        assert_eq!(output["required"], json!(["from"]));
        assert_eq!(output["additionalProperties"], json!(false));
        // The v1 discriminator is gone: an input pipe has no `target`.
        assert!(!input["properties"]
            .as_object()
            .unwrap()
            .contains_key("target"));
    }

    #[test]
    fn pipe_docs_and_schema_defs_list_the_same_fields() {
        // Drift guard: the pipe field lists live in both the serde structs
        // (`format::InputPipeDoc`/`OutputPipeDoc`) and this schema literal, and
        // `additionalProperties: false` means a schema missing a serde field REJECTS documents
        // the loader accepts. The struct literals are exhaustive on purpose — adding a field
        // fails compilation here until the schema (and the committed schema file, via the
        // staleness test) follow.
        let full_input = crate::format::InputPipeDoc {
            ty: "f32".to_string(),
            channel: Some(0),
            default: Some(crate::format::PipeDefault::Number(1.0)),
            min: Some(0.0),
            max: Some(1.0),
            curve: Some(crate::format::CurveDoc::Lin),
            unit: Some("Hz".to_string()),
            label: Some("L".to_string()),
            widget: Some("knob".to_string()),
        };
        let full_output = crate::format::OutputPipeDoc {
            from: "/n.p".to_string(),
            channel: Some(0),
            label: Some("L".to_string()),
            unit: Some("Hz".to_string()),
            widget: Some("knob".to_string()),
            min: Some(0.0),
            max: Some(1.0),
        };
        let schema = generate(&Registry::builtin());
        let fields = |v: serde_json::Value| -> std::collections::BTreeSet<String> {
            v.as_object().expect("object").keys().cloned().collect()
        };
        let schema_fields = |def: &str| -> std::collections::BTreeSet<String> {
            schema["$defs"][def]["properties"]
                .as_object()
                .expect("properties")
                .keys()
                .cloned()
                .collect()
        };
        assert_eq!(
            fields(serde_json::to_value(&full_input).unwrap()),
            schema_fields("inputPipe"),
            "InputPipeDoc (format.rs) and inputPipe (schema.rs) disagree on the field list"
        );
        assert_eq!(
            fields(serde_json::to_value(&full_output).unwrap()),
            schema_fields("outputPipe"),
            "OutputPipeDoc (format.rs) and outputPipe (schema.rs) disagree on the field list"
        );
    }

    #[test]
    fn is_deterministic() {
        assert_eq!(
            generate_pretty(&Registry::builtin()),
            generate_pretty(&Registry::builtin())
        );
    }
}
