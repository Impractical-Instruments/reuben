//! Behavioural tests for the document-manipulation vocabulary, plus the **completeness guard** that
//! makes the vocabulary's derivation from the format mechanical rather than a claim.

use super::*;
use crate::resources::MemoryResolver;
use crate::Registry;
use serde_json::json;

const SRC: &str = "doc.json";

/// A minimal valid two-node instrument: an oscillator into a signal multiply (a gain), tapped to a
/// master output — a realistic-enough graph that removal and rewiring have something to cascade over.
fn seed() -> String {
    json!({
        "format_version": 3,
        "instrument": "test",
        "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": 220.0 } },
            { "type": "mul_f32_signal", "address": "/amp", "inputs": { "a": { "from": "/osc" }, "b": 0.5 } }
        ],
        "interface": {
            "outputs": { "main": { "from": "/amp" } }
        }
    })
    .to_string()
}

fn resolver_with(json: &str) -> MemoryResolver {
    let mut r = MemoryResolver::new();
    r.insert_text(SRC, json);
    r
}

fn readback(resolver: &MemoryResolver) -> serde_json::Value {
    serde_json::from_str(&resolver.resolve_text(SRC).expect("read back")).expect("parse")
}

// --- write-iff-valid -----------------------------------------------------------------------------

#[test]
fn set_input_writes_a_valid_edit_and_returns_a_new_hash() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let before = readback(&resolver);

    let result = set_instrument_input(SRC, "/osc", "freq", json!(440.0), &registry, &resolver)
        .expect("set_input");

    assert!(result.report.ok, "the edit is valid: {:?}", result.report);
    assert!(result.written, "a valid edit is persisted");
    assert!(!result.hash.is_empty());
    // The written document actually changed.
    let after = readback(&resolver);
    assert_ne!(before, after);
    assert_eq!(after["nodes"][0]["inputs"]["freq"], json!(440.0));
    // The echo is the touched node's zoom.
    assert!(
        result.zoom.contains("/osc"),
        "zoom echoes the node: {}",
        result.zoom
    );
}

#[test]
fn an_invalid_edit_is_rejected_and_nothing_is_written() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let before = readback(&resolver);

    // Wiring an input from a node that does not exist is a load error: write-iff-valid refuses it.
    let result = wire_instrument_input(SRC, "/amp", "a", "/nope", &registry, &resolver)
        .expect("call succeeds; the *edit* is what's rejected");

    assert!(!result.report.ok, "the edit is invalid");
    assert!(!result.written, "an invalid edit writes nothing");
    assert!(!result.report.errors.is_empty());
    // The document on disk is untouched.
    assert_eq!(before, readback(&resolver));
}

#[test]
fn a_missing_target_node_is_a_precondition_error_not_a_report() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let err = set_instrument_input(SRC, "/ghost", "freq", json!(1.0), &registry, &resolver)
        .expect_err("no such node");
    assert!(matches!(err, EditError::Target(_)), "got {err:?}");
}

// --- cascade -------------------------------------------------------------------------------------

#[test]
fn remove_node_cascades_and_reports_what_it_broke() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());

    let result = remove_instrument_node(SRC, "/osc", &registry, &resolver).expect("remove");

    assert!(
        result.report.ok,
        "the cascade leaves a valid document: {:?}",
        result.report
    );
    assert!(result.written);
    // /amp's `a` was wired from /osc; the `main` output feeds from /amp (survives). /amp.a must
    // be unwired.
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.contains("/amp.a") && n.contains("/osc")),
        "notes report the unwired consumer: {:?}",
        result.notes
    );
    let after = readback(&resolver);
    assert!(after["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|n| n["address"] != "/osc"));
    // /amp.a is gone (reverted to default), not left dangling.
    assert!(after["nodes"][0]["inputs"].get("a").is_none());
}

#[test]
fn rename_node_rewires_consumers() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());

    let result =
        rename_instrument_node(SRC, "/osc", "/source", &registry, &resolver).expect("rename");

    assert!(result.report.ok, "{:?}", result.report);
    assert!(result.written);
    assert!(
        result.notes.iter().any(|n| n.contains("/source")),
        "notes report the rewire: {:?}",
        result.notes
    );
    let after = readback(&resolver);
    // /amp.a now points at the new address.
    assert_eq!(after["nodes"][1]["inputs"]["a"]["from"], json!("/source"));
}

// --- one-shot add --------------------------------------------------------------------------------

#[test]
fn add_node_lands_fully_formed_with_a_wire() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());

    let mut inputs = BTreeMap::new();
    inputs.insert("a".to_string(), json!({ "from": "/amp" }));
    inputs.insert("b".to_string(), json!(0.8));

    let result = add_instrument_node(
        SRC,
        "/trim",
        "mul_f32_signal",
        inputs,
        BTreeMap::new(),
        Some("output trim"),
        None,
        None,
        None,
        &registry,
        &resolver,
    )
    .expect("add");

    assert!(result.report.ok, "{:?}", result.report);
    assert!(result.written);
    let after = readback(&resolver);
    let trim = after["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["address"] == "/trim")
        .unwrap();
    assert_eq!(trim["type"], json!("mul_f32_signal"));
    assert_eq!(trim["inputs"]["a"]["from"], json!("/amp"));
    assert_eq!(trim["doc"], json!("output trim"));
}

#[test]
fn add_node_at_a_taken_address_is_rejected() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let err = add_instrument_node(
        SRC,
        "/osc",
        "gain",
        BTreeMap::new(),
        BTreeMap::new(),
        None,
        None,
        None,
        None,
        &registry,
        &resolver,
    )
    .expect_err("duplicate address");
    assert!(matches!(err, EditError::Target(_)), "got {err:?}");
}

// --- new_instrument ------------------------------------------------------------------------------

#[test]
fn new_instrument_creates_a_valid_document_and_refuses_to_overwrite() {
    let registry = Registry::builtin();
    let resolver = MemoryResolver::new();

    let result = new_instrument(SRC, "fresh", &registry, &resolver).expect("new");
    assert!(result.report.ok);
    assert!(result.written);
    assert_eq!(readback(&resolver)["instrument"], json!("fresh"));

    // A second new at the same source refuses rather than clobbering.
    let err = new_instrument(SRC, "other", &registry, &resolver).expect_err("no overwrite");
    assert!(matches!(err, EditError::Target(_)), "got {err:?}");
}

// --- interface + resources -----------------------------------------------------------------------

#[test]
fn interface_input_add_and_meta_round_trip() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());

    add_instrument_interface_input(
        SRC,
        "cutoff",
        "f32",
        None,
        Some(json!(1000.0)),
        Some(20.0),
        Some(20000.0),
        Some("exp"),
        Some("Hz"),
        &registry,
        &resolver,
    )
    .expect("add pipe");
    let after = readback(&resolver);
    assert_eq!(after["interface"]["inputs"]["cutoff"]["type"], json!("f32"));
    assert_eq!(
        after["interface"]["inputs"]["cutoff"]["curve"],
        json!("exp")
    );

    let meta = set_instrument_interface_input_meta(
        SRC,
        "cutoff",
        None,
        None,
        Some(50.0),
        None,
        None,
        None,
        &registry,
        &resolver,
    )
    .expect("set meta");
    assert!(meta.written);
    assert_eq!(
        readback(&resolver)["interface"]["inputs"]["cutoff"]["min"],
        json!(50.0)
    );
}

#[test]
fn resource_add_then_remove() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());

    add_instrument_resource(SRC, "kick", "kick.wav", &registry, &resolver).expect("add res");
    assert_eq!(readback(&resolver)["resources"]["kick"], json!("kick.wav"));

    remove_instrument_resource(SRC, "kick", &registry, &resolver).expect("remove res");
    assert!(readback(&resolver)["resources"].get("kick").is_none());
}

// --- the completeness guard ----------------------------------------------------------------------
//
// The write-side mirror of the projection's coverage guard: walk the real `InstrumentDoc` schema
// into its leaf field-paths and set-diff against `VERB_COVERAGE`, so the build goes red the moment
// the format grows a field no verb can reach. The `walk` here mirrors `projection`'s (kept local so
// the two guards stay independent).

#[cfg(feature = "schemars")]
mod coverage {
    use super::*;
    use crate::format::InstrumentDoc;
    use serde_json::{Map, Value};
    use std::collections::BTreeSet;

    /// Walk a schemars (draft 2020-12) schema into a flat, sorted set of leaf field-paths.
    /// Convention: object properties → `parent.field`; arrays → `parent[]`; maps → `parent{}`;
    /// untagged enums / `Option` (`anyOf`/`oneOf`) → union the branches under the same path.
    fn walk(
        node: &Value,
        defs: &Map<String, Value>,
        path: &str,
        out: &mut BTreeSet<String>,
        active: &mut BTreeSet<String>,
    ) {
        if node.get("type").and_then(Value::as_str) == Some("null") {
            return;
        }
        let mut structural = false;
        if let Some(r) = node.get("$ref").and_then(Value::as_str) {
            let name = r.rsplit('/').next().unwrap_or(r).to_string();
            if active.insert(name.clone()) {
                if let Some(def) = defs.get(&name) {
                    walk(def, defs, path, out, active);
                }
                active.remove(&name);
            } else {
                out.insert(format!("{path} → <recursive {name}>"));
            }
            structural = true;
        }
        for key in ["anyOf", "oneOf", "allOf"] {
            for sub in node
                .get(key)
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if sub.get("type").and_then(Value::as_str) == Some("null") {
                    continue;
                }
                walk(sub, defs, path, out, active);
                structural = true;
            }
        }
        for (k, v) in node
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            let child = if path.is_empty() {
                k.clone()
            } else {
                format!("{path}.{k}")
            };
            walk(v, defs, &child, out, active);
            structural = true;
        }
        if let Some(ap) = node.get("additionalProperties") {
            if ap.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
                walk(ap, defs, &format!("{path}{{}}"), out, active);
                structural = true;
            }
        }
        if let Some(items) = node.get("items") {
            walk(items, defs, &format!("{path}[]"), out, active);
            structural = true;
        }
        if !structural {
            out.insert(path.to_string());
        }
    }

    fn format_fields() -> BTreeSet<String> {
        let schema: Value =
            serde_json::to_value(schemars::schema_for!(InstrumentDoc)).expect("schema");
        let defs = schema
            .get("$defs")
            .or_else(|| schema.get("definitions"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut out = BTreeSet::new();
        walk(&schema, &defs, "", &mut out, &mut BTreeSet::new());
        out
    }

    fn undispositioned(fields: &BTreeSet<String>) -> Vec<String> {
        let table: BTreeSet<String> = VERB_COVERAGE.iter().map(|(k, _)| k.to_string()).collect();
        let mut problems: Vec<String> = fields
            .difference(&table)
            .map(|f| format!("UNREACHABLE format field (no verb writes it): {f}"))
            .collect();
        problems.extend(
            table
                .difference(fields)
                .map(|k| format!("STALE coverage row (field no longer in the format): {k}")),
        );
        problems
    }

    #[test]
    fn every_format_field_is_reachable_by_a_verb() {
        let problems = undispositioned(&format_fields());
        assert!(
            problems.is_empty(),
            "the verb vocabulary and the format have diverged.\n\
             Give the new field a verb in `VERB_COVERAGE` (or an explicit `omit:` reason):\n  {}",
            problems.join("\n  ")
        );
    }

    /// Teeth: a future field with no verb goes red. Without this the test above could pass by
    /// walking nothing.
    #[test]
    fn a_new_field_with_no_verb_fails_the_guard() {
        let mut future = format_fields();
        future.insert("nodes[].tempo_hint".to_string());
        assert_eq!(undispositioned(&future).len(), 1);
    }
}
