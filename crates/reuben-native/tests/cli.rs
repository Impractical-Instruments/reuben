//! Integration: the `describe` / `validate` introspection surface the Patcher skill drives
//! (ADR-0020). Exercises the real load + plan path through the public `cli` module.

use std::path::PathBuf;

use reuben_core::Registry;
use reuben_native::cli::{describe, describe_patch, validate};
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
        { "type": "map_f32_signal", "address": "/a", "inputs": { "in": { "from": "/b" } } },
        { "type": "map_f32_signal", "address": "/b", "inputs": { "in": { "from": "/a" } } }
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
    // ADR-0016/0032: a voice resource that doesn't resolve plays silence rather than failing the
    // load. The instrument is still valid (ok), but the unresolved resource surfaces as a warning.
    let json = r#"{
      "instrument": "ghost",
      "resources": { "ghost-voice": "voices/nope.json" },
      "nodes": [
        { "type": "voicer", "address": "/voicer", "voice": "ghost-voice", "config": { "voices": 1 } },
        { "type": "output", "address": "/out", "inputs": { "audio": {"from":"/voicer.audio"} } }
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
    for expected in [
        "oscillator",
        "filter",
        "voicer",
        "output",
        "map_f32_signal",
        "m2s",
    ] {
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

    // A scalar control input carries its range/curve/default inline (ADR-0030).
    let freq = osc
        .inputs
        .iter()
        .find(|p| p.name == "freq")
        .expect("freq input");
    assert_eq!(freq.kind, "signal");
    assert!(freq.default.is_some() && freq.min.is_some() && freq.max.is_some());
    assert_eq!(freq.curve.as_deref(), Some("exponential"));
    assert!(osc
        .outputs
        .iter()
        .any(|p| p.name == "audio" && p.kind == "signal"));
    // `waveform` is an Enum input (ADR-0030) — one input surface, no separate `enums` list; its
    // variants + default symbol ride on the same `PortInfo`.
    let waveform = osc
        .inputs
        .iter()
        .find(|p| p.name == "waveform")
        .expect("waveform input");
    assert_eq!(waveform.kind, "enum");
    assert_eq!(waveform.variants, ["Sine", "Saw"]);
    assert_eq!(waveform.default, Some(serde_json::json!("Sine")));
}

#[test]
fn describe_unknown_operator_errors() {
    let err = describe(&Registry::builtin(), Some("nope")).unwrap_err();
    assert!(
        err.contains("nope"),
        "error should name the missing type: {err}"
    );
}

#[test]
fn describe_patch_surfaces_the_boundary_with_inherited_metadata() {
    // ADR-0034 §4 (P6): a voice patch's `interface` describes as operator-style ports, each
    // inheriting the inner port's type + metadata (default-voice's `freq` targets the
    // oscillator's swept-Hz control, so its range/unit/curve come through).
    let dir = instruments_dir().join("voices");
    let json = std::fs::read_to_string(dir.join("default-voice.json")).expect("read voice");
    let b = describe_patch(&json, &Registry::builtin(), &FsResolver::new(&dir)).expect("describe");

    assert_eq!(b.instrument, "default-voice");
    let freq = b.inputs.iter().find(|p| p.name == "freq").expect("freq");
    assert_eq!(freq.kind, "signal", "type inherited from /osc.freq");
    assert_eq!(freq.unit, "Hz", "unit inherited from the inner port");
    assert!(
        freq.min.is_some() && freq.max.is_some() && freq.default.is_some(),
        "range/default inherited: {freq:?}"
    );
    assert!(
        b.outputs.iter().any(|p| p.name == "audio"),
        "boundary outputs surface: {:?}",
        b.outputs
    );
}

#[test]
fn describe_patch_applies_interface_overrides_but_never_the_type() {
    // ADR-0034 §4: presentational overrides (label/unit/widget/range) decorate the inherited
    // port; the Arg type (`kind`) stays the inner port's truth — there is no way to override it.
    let json = r#"{
      "instrument": "shimmer",
      "interface": {
        "inputs": {
          "brightness": { "target": "/filter.cutoff", "label": "Brightness", "unit": "%",
                          "min": 0, "max": 100, "widget": "knob" }
        },
        "outputs": { "audio": "/filter.audio" }
      },
      "nodes": [ { "type": "filter", "address": "/filter", "inputs": { "cutoff": 2000 } } ]
    }"#;
    let b = describe_patch(json, &Registry::builtin(), &FsResolver::new(".")).expect("describe");

    let p = &b.inputs[0];
    assert_eq!(p.name, "brightness");
    assert_eq!(
        p.kind, "signal",
        "kind is the inner cutoff's, not overridable"
    );
    assert_eq!(p.label.as_deref(), Some("Brightness"));
    assert_eq!(p.unit, "%", "unit override replaces the inner Hz");
    assert_eq!(p.widget.as_deref(), Some("knob"));
    assert_eq!((p.min, p.max), (Some(0.0), Some(100.0)));
    assert_eq!(
        p.curve.as_deref(),
        Some("exponential"),
        "un-overridden fields stay inherited"
    );
    assert_eq!(
        p.default,
        Some(serde_json::json!(2000.0)),
        "the default is the effective unwired value — the child's literal, not the descriptor"
    );
}

#[test]
fn describe_patch_without_interface_yields_an_empty_boundary() {
    let json = r#"{ "instrument": "plain",
      "nodes": [ { "type": "oscillator", "address": "/osc" } ] }"#;
    let b = describe_patch(json, &Registry::builtin(), &FsResolver::new(".")).expect("describe");
    assert!(b.inputs.is_empty() && b.outputs.is_empty());
}
