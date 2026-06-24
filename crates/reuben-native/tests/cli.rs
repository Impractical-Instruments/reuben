//! Integration: the `describe` / `validate` introspection surface the Patcher skill drives
//! (ADR-0020). Exercises the real load + plan path through the public `cli` module.

use std::path::PathBuf;

use reuben_core::Registry;
use reuben_native::cli::{describe, validate};
use reuben_native::resources::FsResolver;

/// Absolute path to the workspace `instruments/` directory, independent of test CWD.
fn instruments_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../instruments")
}

#[test]
fn validate_accepts_a_worked_instrument() {
    let dir = instruments_dir();
    let json =
        std::fs::read_to_string(dir.join("good-button.json")).expect("read good-button.json");
    let report = validate(&json, &Registry::builtin(), &FsResolver::new(&dir));

    assert!(
        report.ok,
        "good-button.json should validate: {:?}",
        report.errors
    );
    assert!(
        report.errors.is_empty(),
        "no errors expected: {:?}",
        report.errors
    );
}

#[test]
fn validate_accepts_the_stereo_autopan_example() {
    let dir = instruments_dir();
    let json =
        std::fs::read_to_string(dir.join("stereo-autopan.json")).expect("read stereo-autopan.json");
    let report = validate(&json, &Registry::builtin(), &FsResolver::new(&dir));
    assert!(
        report.ok && report.errors.is_empty(),
        "stereo-autopan.json should validate: {:?}",
        report.errors
    );
}

#[test]
fn validate_rejects_unknown_operator_and_names_the_node() {
    let json = r#"{
      "instrument": "typo",
      "nodes": [ { "type": "oscilllator", "address": "/osc" } ],
      "connections": [],
      "outputs": []
    }"#;
    let report = validate(json, &Registry::builtin(), &FsResolver::new("."));

    assert!(!report.ok, "unknown operator type should fail validation");
    let err = &report.errors[0];
    assert_eq!(
        err.node.as_deref(),
        Some("/osc"),
        "error should localize the node: {err:?}"
    );
    assert!(
        err.message.contains("oscilllator"),
        "message should name the bad type: {}",
        err.message
    );
}

#[test]
fn validate_rejects_a_cycle_that_loads_cleanly() {
    // Two maps feeding each other: every port/kind is legal, so `load` accepts it — only the
    // plan's topological sort catches the loop. This is why validate instantiates a plan.
    let json = r#"{
      "instrument": "loop",
      "nodes": [
        { "type": "map", "address": "/a" },
        { "type": "map", "address": "/b" }
      ],
      "connections": [
        { "from": {"node":"/a","port":"out"}, "to": {"node":"/b","port":"in"} },
        { "from": {"node":"/b","port":"out"}, "to": {"node":"/a","port":"in"} }
      ],
      "outputs": []
    }"#;
    let report = validate(json, &Registry::builtin(), &FsResolver::new("."));

    assert!(!report.ok, "a cyclic graph should fail validation");
    assert!(
        report.errors[0].message.contains("cycle"),
        "message should mention the cycle: {}",
        report.errors[0].message
    );
}

#[test]
fn validate_treats_a_missing_resource_as_advisory_not_invalid() {
    // ADR-0016: a sample that doesn't resolve plays silence rather than failing the load. The
    // instrument is still valid (ok), but the unresolved sample surfaces as a warning.
    let json = r#"{
      "instrument": "ghost",
      "resources": { "ghost": "samples/nope.wav" },
      "nodes": [
        { "type": "voicer", "address": "/voicer", "params": { "voices": 1 } },
        { "type": "sample", "address": "/s", "sample": "ghost" },
        { "type": "output", "address": "/out" }
      ],
      "connections": [
        { "from": {"node":"/voicer","port":"freq"}, "to": {"node":"/s","port":"freq"} },
        { "from": {"node":"/voicer","port":"gate"}, "to": {"node":"/s","port":"gate"} },
        { "from": {"node":"/s","port":"audio"}, "to": {"node":"/out","port":"audio"} }
      ],
      "outputs": [ {"node":"/out","port":"audio"} ]
    }"#;
    let report = validate(
        json,
        &Registry::builtin(),
        &FsResolver::new(instruments_dir()),
    );

    assert!(
        report.ok,
        "missing resource is advisory, instrument is still valid"
    );
    assert!(
        report.errors.is_empty(),
        "no hard errors: {:?}",
        report.errors
    );
    assert_eq!(
        report.warnings.len(),
        1,
        "the unresolved sample should warn: {:?}",
        report.warnings
    );
}

#[test]
fn describe_lists_every_registered_operator() {
    let reg = Registry::builtin();
    let ops = describe(&reg, None).expect("describe all");

    let names: Vec<&str> = ops.iter().map(|o| o.type_name.as_str()).collect();
    for expected in ["oscillator", "filter", "voicer", "output", "map", "m2s"] {
        assert!(names.contains(&expected), "missing {expected} in {names:?}");
    }
    assert_eq!(
        ops.len(),
        reg.type_names().count(),
        "describe lists exactly the registry"
    );
}

#[test]
fn describe_one_operator_surfaces_its_ports_and_params() {
    let ops = describe(&Registry::builtin(), Some("oscillator")).expect("describe oscillator");
    assert_eq!(ops.len(), 1);
    let osc = &ops[0];

    let input = osc
        .inputs
        .iter()
        .find(|p| p.name == "freq")
        .expect("freq input");
    assert_eq!(input.kind, "signal");
    assert!(osc
        .outputs
        .iter()
        .any(|p| p.name == "audio" && p.kind == "signal"));
    assert!(
        osc.params.iter().any(|p| p.name == "waveform"),
        "waveform param"
    );
}

#[test]
fn describe_unknown_operator_errors() {
    let err = describe(&Registry::builtin(), Some("nope")).unwrap_err();
    assert!(
        err.contains("nope"),
        "error should name the missing type: {err}"
    );
}
