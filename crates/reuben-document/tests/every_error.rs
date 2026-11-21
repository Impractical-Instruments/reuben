//! The loader reports every error a document has in one load, in document order, and one root
//! fault once: whatever depends on it is skipped, not reported again.

use reuben_core::registry::Registry;
use reuben_document::introspect::validate;
use reuben_document::{load_instrument, Diag, LoadError, MemoryResolver};
use serde_json::{json, Value};

fn errors(doc: &Value, resolver: &MemoryResolver) -> Vec<Diag> {
    let report = validate(&doc.to_string(), &Registry::builtin(), resolver);
    assert_eq!(report.ok, report.errors.is_empty(), "{report:?}");
    report.errors
}

fn loci(errors: &[Diag]) -> Vec<(Option<&str>, Option<&str>)> {
    errors
        .iter()
        .map(|d| (d.node.as_deref(), d.port.as_deref()))
        .collect()
}

fn instrument(nodes: Value, interface: Value) -> Value {
    json!({ "format_version": 3, "instrument": "t", "nodes": nodes, "interface": interface })
}

/// Three nodes, each with its own unrelated fault, plus an interface output fed from nowhere.
fn three_faults() -> Value {
    instrument(
        json!([
            { "type": "oscillator", "address": "/a", "inputs": { "bogus": 1.0 } },
            { "type": "mul_f32_signal", "address": "/b", "inputs": { "a": { "from": "/ghost" } } },
            { "type": "oscillator", "address": "/c", "inputs": { "waveform": "Squarish" } }
        ]),
        json!({ "outputs": { "main": { "from": "/nobody" } } }),
    )
}

#[test]
fn independent_faults_on_different_nodes_are_all_reported() {
    let errors = errors(&three_faults(), &MemoryResolver::new());
    assert_eq!(
        loci(&errors),
        vec![
            (None, Some("main")),
            (Some("/a"), Some("bogus")),
            (Some("/b"), Some("a")),
            (Some("/c"), Some("waveform")),
        ],
        "{errors:?}"
    );
}

#[test]
fn the_order_is_the_documents_not_the_loaders() {
    // The wire fault on `/b` is found in a later pass than the literal fault on `/c`, but `/b`
    // comes first in the document, so it is reported first.
    let doc = instrument(
        json!([
            { "type": "mul_f32_signal", "address": "/b", "inputs": { "a": { "from": "/ghost" } } },
            { "type": "oscillator", "address": "/c", "inputs": { "waveform": "Squarish" } }
        ]),
        json!({}),
    );
    let resolver = MemoryResolver::new();
    let first = errors(&doc, &resolver);
    assert_eq!(
        loci(&first),
        vec![(Some("/b"), Some("a")), (Some("/c"), Some("waveform"))],
        "{first:?}"
    );

    let mut swapped = doc.clone();
    swapped["nodes"].as_array_mut().unwrap().reverse();
    assert_eq!(
        loci(&errors(&swapped, &resolver)),
        vec![(Some("/c"), Some("waveform")), (Some("/b"), Some("a"))],
    );
    assert_eq!(first, errors(&doc, &resolver), "one document, one report");
}

#[test]
fn within_a_node_the_errors_follow_the_check_order() {
    let doc = instrument(
        json!([
            { "type": "voicer", "address": "/v", "config": { "nope": 1 },
              "inputs": { "voices": 2, "bogus": 1.0 } }
        ]),
        json!({}),
    );
    let errors = errors(&doc, &MemoryResolver::new());
    assert_eq!(errors.len(), 3, "{errors:?}");
    assert!(errors.iter().all(|d| d.node.as_deref() == Some("/v")));
}

#[test]
fn an_unknown_operator_type_is_reported_once() {
    // Every fault but `/ghost` hangs off `/x`, whose ports nothing can know: its own literal, the
    // wires out of it, and the interface output it feeds. What its wire names is another matter.
    let doc = instrument(
        json!([
            { "type": "nosuchoperator", "address": "/x",
              "inputs": { "bogus": 1.0, "a": { "from": "/ghost" } } },
            { "type": "mul_f32_signal", "address": "/y", "inputs": { "a": { "from": "/x.out" } } },
            { "type": "mul_f32_signal", "address": "/z", "inputs": { "a": { "from": "/x" } } }
        ]),
        json!({ "outputs": { "main": { "from": "/x.out" } } }),
    );
    let errors = errors(&doc, &MemoryResolver::new());
    assert_eq!(
        loci(&errors),
        vec![(Some("/x"), None), (Some("/x"), Some("a"))],
        "{errors:?}"
    );
    assert!(errors[0].message.contains("nosuchoperator"), "{errors:?}");
    assert!(errors[1].message.contains("/ghost"), "{errors:?}");
}

#[test]
fn a_malformed_input_pipe_is_reported_once() {
    let doc = instrument(
        json!([
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/cutoff" } } }
        ]),
        json!({
            "inputs": { "cutoff": { "type": "no_such_type" } },
            "outputs": { "main": { "from": "/cutoff" } }
        }),
    );
    let errors = errors(&doc, &MemoryResolver::new());
    assert_eq!(loci(&errors), vec![(None, Some("cutoff"))], "{errors:?}");
}

#[test]
fn a_malformed_input_pipe_still_holds_its_address() {
    let doc = instrument(
        json!([
            { "type": "oscillator", "address": "/cutoff", "inputs": { "freq": { "from": "/ghost" } } }
        ]),
        json!({ "inputs": { "cutoff": { "type": "no_such_type" } } }),
    );
    let errors = errors(&doc, &MemoryResolver::new());
    assert_eq!(
        loci(&errors),
        vec![
            (None, Some("cutoff")),
            (Some("/cutoff"), None),
            (Some("/cutoff"), Some("freq"))
        ],
        "{errors:?}"
    );
    assert!(errors[1].message.contains("duplicate"), "{errors:?}");
}

#[test]
fn a_root_fault_and_an_independent_fault_are_two_errors() {
    let doc = instrument(
        json!([
            { "type": "nosuchoperator", "address": "/x" },
            { "type": "mul_f32_signal", "address": "/y", "inputs": { "a": { "from": "/x" } } },
            { "type": "oscillator", "address": "/c", "inputs": { "waveform": "Squarish" } }
        ]),
        json!({}),
    );
    let errors = errors(&doc, &MemoryResolver::new());
    assert_eq!(
        loci(&errors),
        vec![(Some("/x"), None), (Some("/c"), Some("waveform"))],
        "{errors:?}"
    );
}

#[test]
fn a_wire_into_an_unknown_input_from_an_unknown_node_is_one_fault() {
    let doc = instrument(
        json!([
            { "type": "oscillator", "address": "/osc", "inputs": { "bogus": { "from": "/ghost" } } }
        ]),
        json!({}),
    );
    let errors = errors(&doc, &MemoryResolver::new());
    assert_eq!(
        loci(&errors),
        vec![(Some("/osc"), Some("bogus"))],
        "{errors:?}"
    );
}

#[test]
fn a_duplicate_address_does_not_check_the_second_nodes_wires_against_the_first() {
    let doc = instrument(
        json!([
            { "type": "oscillator", "address": "/n" },
            { "type": "mul_f32_signal", "address": "/n", "inputs": { "a": { "from": "/src" } } },
            { "type": "oscillator", "address": "/src" }
        ]),
        json!({}),
    );
    let errors = errors(&doc, &MemoryResolver::new());
    assert_eq!(loci(&errors), vec![(Some("/n"), None)], "{errors:?}");
}

/// A child with two faults of its own.
const BROKEN_CHILD: &str = r#"{ "format_version": 3, "instrument": "fx",
    "nodes": [
        { "type": "oscillator", "address": "/o", "inputs": { "bogus": 1.0 } },
        { "type": "oscillator", "address": "/p", "inputs": { "waveform": "Squarish" } }
    ],
    "interface": { "outputs": { "out": { "from": "/o" } } } }"#;

#[test]
fn a_broken_child_is_reported_on_each_node_that_uses_it_and_nowhere_else() {
    let mut resolver = MemoryResolver::new();
    resolver.insert_text("fx.json", BROKEN_CHILD);
    let doc = json!({
        "format_version": 3, "instrument": "t",
        "resources": { "fx": "fx.json" },
        "nodes": [
            { "type": "subpatch", "address": "/one", "patch": "fx", "inputs": { "nope": 1.0 } },
            { "type": "subpatch", "address": "/two", "patch": "fx" },
            { "type": "mul_f32_signal", "address": "/amp", "inputs": { "a": { "from": "/two.out" } } },
            { "type": "oscillator", "address": "/c", "inputs": { "waveform": "Squarish" } }
        ],
        "interface": { "outputs": { "main": { "from": "/one.out" } } }
    });
    let errors = errors(&doc, &resolver);
    let messages: Vec<&str> = errors.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(errors.len(), 5, "{messages:#?}");
    for (at, host) in [(0, "/one"), (2, "/two")] {
        assert!(
            messages[at].starts_with(&format!("in {host:?}:")),
            "{messages:#?}"
        );
        assert!(messages[at].contains("bogus"), "{messages:#?}");
        assert!(messages[at + 1].contains("Squarish"), "{messages:#?}");
    }
    assert_eq!(loci(&errors[4..]), vec![(Some("/c"), Some("waveform"))]);
}

#[test]
fn a_broken_voice_reports_its_own_errors_once() {
    let mut resolver = MemoryResolver::new();
    resolver.insert_text("fx.json", BROKEN_CHILD);
    let doc = json!({
        "format_version": 3, "instrument": "t",
        "resources": { "fx": "fx.json" },
        "nodes": [
            { "type": "voicer", "address": "/v", "voice": "fx", "config": { "voices": 4 } },
            { "type": "oscillator", "address": "/c", "inputs": { "waveform": "Squarish" } }
        ]
    });
    let errors = errors(&doc, &resolver);
    assert_eq!(errors.len(), 3, "{errors:#?}");
}

#[test]
fn the_load_itself_returns_every_error() {
    let err = match load_instrument(
        &three_faults().to_string(),
        &Registry::builtin(),
        &MemoryResolver::new(),
    ) {
        Ok(_) => panic!("three faults must not load"),
        Err(err) => err,
    };
    assert_eq!(err.len(), 4);
    assert!(matches!(err.first(), LoadError::UnknownNode { .. }));
    let kinds: Vec<_> = err.iter().map(std::mem::discriminant).collect();
    assert_eq!(kinds.len(), 4);
}

const BROKEN_TYPE_CHILD: &str = r#"{ "format_version": 3, "instrument": "c",
    "nodes": [ { "type": "nope", "address": "/x" } ] }"#;

fn with_child(child: &str) -> MemoryResolver {
    let mut resolver = MemoryResolver::new();
    resolver.insert_text("c.json", child);
    resolver
}

#[test]
fn a_broken_child_is_reported_on_every_node_that_references_it() {
    // Each reference is its own fault in this document: removing one leaves the other's error
    // exactly where it was.
    let doc = json!({
        "format_version": 3, "instrument": "t",
        "resources": { "c": "c.json" },
        "nodes": [
            { "type": "voicer", "address": "/v", "voice": "c" },
            { "type": "subpatch", "address": "/s", "patch": "c" },
            { "type": "subpatch", "address": "/t", "patch": "c" }
        ]
    });
    let errors = errors(&doc, &with_child(BROKEN_TYPE_CHILD));
    assert_eq!(
        loci(&errors),
        vec![(Some("/v"), None), (Some("/s"), None), (Some("/t"), None)],
        "{errors:#?}"
    );
}

#[test]
fn a_broken_child_does_not_hide_a_bind_key_on_another_reference() {
    let doc = json!({
        "format_version": 3, "instrument": "t",
        "resources": { "c": "c.json", "k": "k.wav" },
        "nodes": [
            { "type": "subpatch", "address": "/a", "patch": "c" },
            { "type": "subpatch", "address": "/b", "patch": "c", "bind": { "nope": "k" } }
        ]
    });
    let errors = errors(&doc, &with_child(BROKEN_TYPE_CHILD));
    assert!(
        errors
            .iter()
            .any(|d| d.message.contains("bind key \"nope\"")),
        "{errors:#?}"
    );
}

#[test]
fn a_voice_on_a_node_with_no_voice_slot_is_one_error() {
    let doc = json!({
        "format_version": 3, "instrument": "t",
        "resources": { "c": "c.json" },
        "nodes": [ { "type": "oscillator", "address": "/o", "voice": "c" } ]
    });
    let errors = errors(&doc, &with_child(BROKEN_TYPE_CHILD));
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert!(errors[0].message.contains("resource slot"), "{errors:#?}");
}

#[test]
fn a_wire_source_is_checked_whatever_is_wrong_with_the_node_holding_it() {
    for node in [
        json!([{ "type": "oscillator", "address": "/o sc", "inputs": { "freq": { "from": "/ghost" } } }]),
        json!([
            { "type": "oscillator", "address": "/o" },
            { "type": "oscillator", "address": "/o", "inputs": { "freq": { "from": "/ghost" } } }
        ]),
        json!([{ "type": "nope", "address": "/o", "inputs": { "freq": { "from": "/ghost" } } }]),
    ] {
        let mut doc = instrument(node, json!({}));
        doc["format_version"] = json!(4);
        let errors = errors(&doc, &MemoryResolver::new());
        assert_eq!(errors.len(), 2, "{errors:#?}");
        assert!(errors[1].message.contains("/ghost"), "{errors:#?}");
    }
}

#[test]
fn the_structure_read_pins_a_broken_child_on_a_node_that_has_other_errors_too() {
    let doc = json!({
        "format_version": 3, "instrument": "t",
        "resources": { "c": "c.json" },
        "nodes": [
            { "type": "subpatch", "address": "/a", "patch": "c" },
            { "type": "subpatch", "address": "/b", "patch": "c", "config": { "bogus": 1 } }
        ]
    });
    let registry = Registry::builtin();
    let resolver = with_child(BROKEN_TYPE_CHILD);
    let doc = reuben_document::NormalizedDoc::from_json(&doc.to_string(), &registry, None)
        .expect("parses");
    let read = reuben_document::structure::read(&doc, &registry, &resolver);
    let on_b: Vec<&str> = (read.report.errors.iter())
        .filter(|d| d.node.as_deref() == Some("/b"))
        .map(|d| d.message.as_str())
        .collect();
    assert!(on_b.iter().any(|m| m.contains("nope")), "{on_b:#?}");
    assert!(on_b.iter().any(|m| m.contains("bogus")), "{on_b:#?}");
    let on_a = (read.report.errors.iter()).filter(|d| d.node.as_deref() == Some("/a"));
    assert_eq!(on_a.count(), 1, "{:#?}", read.report.errors);
}

fn mul(address: &str, from: &str) -> Value {
    json!({ "type": "mul_f32_signal", "address": address, "inputs": { "a": { "from": from } } })
}

/// A cycle is found from the wires that resolve even when the load fails for another reason, and
/// is located on the nodes it runs through.
#[test]
fn a_cycle_is_reported_beside_another_fault_on_the_nodes_it_runs_through() {
    let doc = instrument(
        json!([mul("/a", "/b"), mul("/b", "/a"), mul("/c", "/ghost")]),
        json!({}),
    );
    let errors = errors(&doc, &MemoryResolver::new());
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert_eq!(loci(&errors)[0], (Some("/c"), Some("a")), "{errors:?}");
    assert_eq!(errors[1].node.as_deref(), Some("/a"), "{errors:?}");
    assert!(
        errors[1].message.contains("cycle") && errors[1].message.contains("/a, /b"),
        "{errors:?}"
    );
}

/// Two separate cycles are two errors, each naming only its own nodes; a node downstream of a
/// cycle is on none.
#[test]
fn each_cycle_is_its_own_error_and_names_only_its_own_nodes() {
    let doc = instrument(
        json!([
            mul("/a", "/b"),
            mul("/b", "/a"),
            mul("/tail", "/a"),
            mul("/x", "/x")
        ]),
        json!({}),
    );
    let errors = errors(&doc, &MemoryResolver::new());
    let messages: Vec<&str> = errors.iter().map(|d| d.message.as_str()).collect();
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        messages[0].contains("/a, /b") && !messages[0].contains("/tail"),
        "{messages:?}"
    );
    assert!(messages[1].contains("/x"), "{messages:?}");
}

/// A cycle through a `subpatch` is located on the node that holds the child, never on the child's
/// own addresses.
#[test]
fn a_cycle_through_a_subpatch_is_located_on_the_node_that_holds_it() {
    let resolver = with_child(
        &json!({
            "format_version": 3, "instrument": "c",
            "interface": {
                "inputs": { "in": { "type": "f32_buffer" } },
                "outputs": { "out": { "from": "/g" } }
            },
            "nodes": [mul("/g", "/in")]
        })
        .to_string(),
    );
    let doc = json!({
        "format_version": 3, "instrument": "t",
        "resources": { "c": "c.json" },
        "nodes": [
            { "type": "subpatch", "address": "/fx", "patch": "c", "inputs": { "in": { "from": "/a" } } },
            mul("/a", "/fx.out")
        ]
    });
    let errors = errors(&doc, &resolver);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].message.contains("/a, /fx"), "{errors:?}");
}
