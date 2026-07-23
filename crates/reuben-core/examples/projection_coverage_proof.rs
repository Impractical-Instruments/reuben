//! THROWAWAY feasibility tracer for wayfinder #600 (the structural projection).
//!
//! Proves the load-bearing claim behind the "lossless-in-aggregate + CI completeness guard"
//! principle: **every field of the instrument document format can be enumerated mechanically**,
//! so a guard can fail the build the moment a new format field is not consciously dispositioned
//! into some projection view. Not the real projection — just the guard's skeleton, run against
//! the real `InstrumentDoc` via schemars.
//!
//! Run: `cargo run -p reuben-core --example projection_coverage_proof --features schemars`

use serde_json::{Map, Value};
use std::collections::BTreeSet;

use reuben_core::format::InstrumentDoc;

/// Walk a schemars (draft 2020-12) schema into a flat, sorted set of leaf field-paths.
/// Convention: object properties → `parent.field`; arrays → `parent[]`; maps
/// (`additionalProperties` schema) → `parent{}`; untagged enums / `Option` (`anyOf`/`oneOf`) →
/// union the branches under the same path. `$ref` resolves into `$defs`, with a visited guard so a
/// (hypothetical) recursive type terminates.
fn walk(
    node: &Value,
    defs: &Map<String, Value>,
    path: &str,
    out: &mut BTreeSet<String>,
    active: &mut BTreeSet<String>,
) {
    // A `{"type": "null"}` branch (the None side of an Option) carries no field — drop it.
    if node.get("type").and_then(Value::as_str) == Some("null") {
        return;
    }

    // $ref → resolve into $defs.
    if let Some(r) = node.get("$ref").and_then(Value::as_str) {
        let name = r.rsplit('/').next().unwrap_or(r).to_string();
        if !active.insert(name.clone()) {
            out.insert(format!("{path} → <recursive {name}>"));
            return;
        }
        if let Some(def) = defs.get(&name) {
            walk(def, defs, path, out, active);
        }
        active.remove(&name);
        return;
    }

    // Untagged enums / Option / boxed metadata: union every non-null branch under this path.
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(arr) = node.get(key).and_then(Value::as_array) {
            let mut recursed = false;
            for sub in arr {
                if sub.get("type").and_then(Value::as_str) == Some("null") {
                    continue;
                }
                walk(sub, defs, path, out, active);
                recursed = true;
            }
            if recursed {
                return;
            }
        }
    }

    // Struct: recurse each property, extending the path.
    if let Some(props) = node.get("properties").and_then(Value::as_object) {
        for (k, v) in props {
            let child = if path.is_empty() {
                k.clone()
            } else {
                format!("{path}.{k}")
            };
            walk(v, defs, &child, out, active);
        }
        return;
    }

    // Map (BTreeMap<String, V>): `additionalProperties` is the value schema.
    if let Some(ap) = node.get("additionalProperties") {
        if ap.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            walk(ap, defs, &format!("{path}{{}}"), out, active);
            return;
        }
    }

    // Array (Vec<V>): `items` is the element schema.
    if let Some(items) = node.get("items") {
        walk(items, defs, &format!("{path}[]"), out, active);
        return;
    }

    // Leaf.
    out.insert(path.to_string());
}

/// Each enumerated field-path's conscious disposition. THIS is what a real guard forces a human to
/// fill in for every format field — into a projection view, or an explicit, reasoned omission.
/// Filled here by hand to model the guard; a red build results the moment enumeration and this
/// table disagree.
const COVERAGE: &[(&str, &str)] = &[
    // --- top level ---
    ("format_version", "index"),
    ("instrument", "index"),
    ("doc", "node-zoom"), // authorial intent, on demand — not in the flat index
    ("resources{}", "resources-view"), // id → path table
    ("nodes[].type", "index"),
    ("nodes[].address", "index"),
    ("nodes[].doc", "node-zoom"),
    ("nodes[].config{}", "node-zoom"), // instantiate-time constants (distinct verb from inputs!)
    ("nodes[].inputs{}", "node-zoom"), // Symbol/Number literal branch of the untagged InputValue
    ("nodes[].inputs{}.from", "node-zoom"), // Wire branch — the inbound source (crux gap I flagged)
    ("nodes[].sample", "resources-view"),
    ("nodes[].voice", "resources-view"),
    ("nodes[].patch", "resources-view"),
    (
        "nodes[].control",
        "omit:retired deserialize-only sink (drained to a deprecation warning)",
    ),
    // v1-only master-tap list; migrated away, never in a v2+ doc
    (
        "outputs[].node",
        "omit:v1-only, migrated into interface.outputs",
    ),
    (
        "outputs[].port",
        "omit:v1-only, migrated into interface.outputs",
    ),
    (
        "outputs[].channel",
        "omit:v1-only, migrated into interface.outputs",
    ),
    // --- interface: input pipes (InterfaceEntry union → InputPipeDoc fields) ---
    (
        "interface.inputs{}",
        "omit:v1-only bare Target string form (migrated at parse)",
    ),
    ("interface.inputs{}.type", "pipe-view"),
    ("interface.inputs{}.channel", "pipe-view"),
    ("interface.inputs{}.default", "pipe-view"),
    ("interface.inputs{}.min", "pipe-view"), // range — what #575's nudge depends on
    ("interface.inputs{}.max", "pipe-view"),
    ("interface.inputs{}.curve", "pipe-view"),
    ("interface.inputs{}.unit", "pipe-view"),
    // InterfaceEntry is one untagged union, so BOTH maps enumerate every variant's fields:
    ("interface.inputs{}.from", "pipe-view"), // Feed variant
    ("interface.inputs{}.target", "omit:v1-only migration form"),
    (
        "interface.inputs{}.label",
        "omit:retired presentation, moved to surface docs",
    ),
    (
        "interface.inputs{}.widget",
        "omit:retired presentation, moved to surface docs",
    ),
    // --- interface: output pipes (same union) ---
    (
        "interface.outputs{}",
        "omit:v1-only bare Target string form (migrated at parse)",
    ),
    ("interface.outputs{}.type", "pipe-view"),
    ("interface.outputs{}.channel", "pipe-view"),
    ("interface.outputs{}.default", "pipe-view"),
    ("interface.outputs{}.min", "pipe-view"),
    ("interface.outputs{}.max", "pipe-view"),
    ("interface.outputs{}.curve", "pipe-view"),
    ("interface.outputs{}.unit", "pipe-view"),
    ("interface.outputs{}.from", "pipe-view"),
    ("interface.outputs{}.target", "omit:v1-only migration form"),
    (
        "interface.outputs{}.label",
        "omit:retired presentation, moved to surface docs",
    ),
    (
        "interface.outputs{}.widget",
        "omit:retired presentation, moved to surface docs",
    ),
];

fn run_guard(enumerated: &BTreeSet<String>, coverage_keys: &BTreeSet<String>) -> Vec<String> {
    let mut problems = Vec::new();
    for f in enumerated.difference(coverage_keys) {
        problems.push(format!(
            "  UNDISPOSITIONED format field (silent-omission risk): {f}"
        ));
    }
    for k in coverage_keys.difference(enumerated) {
        problems.push(format!(
            "  STALE coverage entry (field no longer in format): {k}"
        ));
    }
    problems
}

fn main() {
    let schema: Value = serde_json::to_value(schemars::schema_for!(InstrumentDoc)).expect("schema");
    let defs = schema
        .get("$defs")
        .or_else(|| schema.get("definitions"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let mut enumerated = BTreeSet::new();
    let mut active = BTreeSet::new();
    walk(&schema, &defs, "", &mut enumerated, &mut active);

    println!("=== Mechanically enumerated format field-paths (from the real InstrumentDoc) ===");
    for f in &enumerated {
        println!("  {f}");
    }
    println!(
        "  ---> {} leaf fields, enumerated with zero hand-maintenance\n",
        enumerated.len()
    );

    let coverage_keys: BTreeSet<String> = COVERAGE.iter().map(|(k, _)| k.to_string()).collect();

    println!("=== Guard run #1: real format vs the coverage table ===");
    let problems = run_guard(&enumerated, &coverage_keys);
    if problems.is_empty() {
        println!(
            "  PASS — every one of the {} format fields is consciously dispositioned.\n",
            enumerated.len()
        );
    } else {
        println!("  FAIL:\n{}", problems.join("\n"));
        std::process::exit(1);
    }

    // --- Prove the guard has teeth: simulate a future dev adding a field and forgetting the projection. ---
    println!(
        "=== Guard run #2: simulate a new field `nodes[].tempo_hint` landing with no view ==="
    );
    let mut future = enumerated.clone();
    future.insert("nodes[].tempo_hint".to_string());
    let problems = run_guard(&future, &coverage_keys);
    if problems.is_empty() {
        println!("  UNEXPECTED PASS — the guard is toothless.");
        std::process::exit(1);
    }
    println!("  Caught, build would go red:\n{}\n", problems.join("\n"));

    println!("PROVEN: silent omission is converted into a mechanical build failure.");
}
