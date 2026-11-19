//! Behavioural tests for the document-manipulation vocabulary: what each verb writes, and what a
//! removal or a rename cascades into.

use super::*;
use crate::resources::MemoryResolver;
use reuben_core::Registry;
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

/// [`seed`] plus a declared interface **input** pipe that `/osc.freq` consumes — the shape that
/// makes a pipe removal or a colliding rename dangle, since the pipe mints `/cutoff`.
fn seed_with_pipe() -> String {
    json!({
        "format_version": 3,
        "instrument": "test",
        "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/cutoff" } } },
            { "type": "mul_f32_signal", "address": "/amp", "inputs": { "a": { "from": "/osc" }, "b": 0.5 } }
        ],
        "interface": {
            "inputs": { "cutoff": { "type": "f32", "min": 20.0, "max": 20000.0, "default": 440.0 } },
            "outputs": { "main": { "from": "/amp" } }
        }
    })
    .to_string()
}

/// [`seed`] with the one operator carrying a plan-time `Constant` — the only shape
/// `set_instrument_constant` can be exercised on against a document that still validates.
fn seed_with_voicer() -> String {
    json!({
        "format_version": 3,
        "instrument": "test",
        "nodes": [
            { "type": "voicer", "address": "/voices" },
            { "type": "mul_f32_signal", "address": "/amp", "inputs": { "a": { "from": "/voices" }, "b": 0.5 } }
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
    assert!(
        result.zoom.contains("/osc"),
        "the echo names what was edited: {}",
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

// --- one address space: a pipe is a node ---------------------------------------------------------

/// An `interface.inputs` entry **is a node** — it mints `/<name>` and behaves like a source — so its
/// seed is set through the same verb and the same address space as any other node input.
#[test]
fn set_input_seeds_a_pipe_through_the_address_it_mints() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_pipe());

    let result = set_instrument_input(SRC, "/cutoff", "in", json!(880.0), &registry, &resolver)
        .expect("seed");

    assert!(result.report.ok, "{:?}", result.report);
    assert!(result.written);
    assert_eq!(
        readback(&resolver)["interface"]["inputs"]["cutoff"]["default"],
        json!(880.0),
        "the seed lands on the pipe's `default`, its disk spelling"
    );
}

/// A pipe is a single-port pass-through, so any port but `in` is a precondition error that names
/// the one port there is rather than writing somewhere the caller did not mean.
#[test]
fn a_pipe_takes_only_its_one_input() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_pipe());

    let err = set_instrument_input(
        SRC,
        "/cutoff",
        "default",
        json!(880.0),
        &registry,
        &resolver,
    )
    .expect_err("a pipe has no `default` port");
    assert!(err.to_string().contains('`'), "{err}");
    assert!(
        err.to_string().contains("`in`"),
        "the error names the port to use: {err}"
    );
}

/// The address space is one; the operations over it are not. Every verb that addresses an input
/// **resolves** a pipe address — none of them may report it absent, which is the whole premise of
/// the shared namespace — and a verb whose operation is meaningless on a boundary input refuses in
/// terms of what the address *is*.
#[test]
fn every_input_verb_resolves_a_pipe_address_and_refuses_in_its_terms() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_pipe());

    set_instrument_input(SRC, "/cutoff", "in", json!(880.0), &registry, &resolver)
        .expect("setting a value is meaningful on a boundary input");

    let refusals = [
        (
            "wire",
            wire_instrument_input(SRC, "/cutoff", "in", "/osc", &registry, &resolver).unwrap_err(),
            "boundary",
        ),
        (
            "unwire",
            unwire_instrument_input(SRC, "/cutoff", "in", &registry, &resolver).unwrap_err(),
            "no wire to clear",
        ),
        (
            "set_constant",
            set_instrument_constant(SRC, "/cutoff", "voices", json!(4), &registry, &resolver)
                .unwrap_err(),
            "`config`",
        ),
        (
            "remove_constant",
            remove_instrument_constant(SRC, "/cutoff", "voices", &registry, &resolver).unwrap_err(),
            "`config`",
        ),
    ];
    for (verb, err, why) in refusals {
        let message = err.to_string();
        assert!(matches!(err, EditError::Target(_)), "{verb}: {err:?}");
        assert!(
            !message.contains("no node at address") && !message.contains("no node or interface"),
            "{verb} must not report a real address as absent: {message}"
        );
        assert!(
            message.contains("`cutoff`") && message.contains("interface input pipe"),
            "{verb} names what the address is: {message}"
        );
        assert!(
            message.contains(why),
            "{verb} says why its operation is meaningless here: {message}"
        );
    }

    // Wiring a consumer *from* the pipe is ordinary and untouched — it is being wired *into* that
    // a boundary input refuses.
    wire_instrument_input(SRC, "/amp", "b", "/cutoff", &registry, &resolver)
        .expect("a pipe is a source like any other");

    // And a genuinely absent address still reads as absent, so the two failures stay tellable apart.
    let absent = wire_instrument_input(SRC, "/ghost", "in", "/osc", &registry, &resolver)
        .expect_err("no such address");
    assert!(absent.to_string().contains("/ghost"), "{absent}");
}

/// The node verbs share the namespace too: a pipe address is declared rather than added, so they
/// name the interface half of the vocabulary instead of denying the address exists.
#[test]
fn the_node_verbs_send_a_pipe_address_to_the_interface_verbs() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_pipe());

    let err = remove_instrument_node(SRC, "/cutoff", &registry, &resolver).expect_err("not a node");
    let message = err.to_string();
    assert!(
        !message.contains("no node at address"),
        "the address is real: {message}"
    );
    assert!(
        message.contains("remove_instrument_interface_input"),
        "the refusal names the verb that does reach it: {message}"
    );
}

// --- the wire is not severed by a value edit ------------------------------------------------------

/// Setting a literal on an input that currently holds a wire is **refused**, not silently applied:
/// in the shipped library a vocabulary-target input fed from an interface pipe is the common case,
/// and a destroyed wire is not recoverable from the result. The error names the way through.
#[test]
fn set_input_refuses_to_sever_a_wire() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_pipe());
    let before = readback(&resolver);

    let err = set_instrument_input(SRC, "/osc", "freq", json!(440.0), &registry, &resolver)
        .expect_err("`/osc.freq` is wired from `/cutoff`");

    assert!(matches!(err, EditError::Target(_)), "got {err:?}");
    let message = err.to_string();
    assert!(
        message.contains("/cutoff"),
        "the refusal names the wire it would have severed: {message}"
    );
    assert!(
        message.contains("unwire_instrument_input"),
        "the refusal names the verb that severs on purpose: {message}"
    );
    assert_eq!(
        before,
        readback(&resolver),
        "the wired input is not clobbered"
    );
}

// --- the value verbs echo the change --------------------------------------------------------------

/// A value edit's echo is `from → to`, not the state it landed in: a zoom shows `freq=440` and
/// structurally cannot show what it was, because the prior document is gone.
#[test]
fn the_value_verbs_echo_the_change_not_the_state() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());

    let input = set_instrument_input(SRC, "/osc", "freq", json!(440.0), &registry, &resolver)
        .expect("set input");
    assert_eq!(input.zoom, "/osc.freq 220 → 440");

    let voicer = resolver_with(&seed_with_voicer());
    let constant = set_instrument_constant(SRC, "/voices", "voices", json!(4), &registry, &voicer)
        .expect("set constant");
    assert!(constant.report.ok && constant.written, "{constant:?}");
    assert_eq!(
        constant.zoom, "/voices.voices (unset) → 4",
        "a slot that held nothing says so rather than inventing a prior value"
    );

    let pipe_resolver = resolver_with(&seed_with_pipe());
    let seeded = set_instrument_input(
        SRC,
        "/cutoff",
        "in",
        json!(880.0),
        &registry,
        &pipe_resolver,
    )
    .expect("seed the pipe");
    assert_eq!(seeded.zoom, "/cutoff.in 440 → 880");
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

/// A pipe mints `/<name>` into the node namespace, so removing a *consumed* pipe dangles exactly
/// like removing a consumed node — and gets the same cascade rather than a rejected write.
#[test]
fn remove_interface_input_cascades_over_its_minted_address() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_pipe());

    let result = remove_instrument_interface_input(SRC, "cutoff", &registry, &resolver)
        .expect("remove pipe");

    assert!(
        result.report.ok,
        "the cascade leaves a valid document: {:?}",
        result.report
    );
    assert!(result.written, "the cascade is written, not rejected");
    assert!(
        result
            .notes
            .iter()
            .any(|n| n.contains("/osc.freq") && n.contains("/cutoff")),
        "notes report the unwired consumer: {:?}",
        result.notes
    );
    let after = readback(&resolver);
    assert!(after["interface"]["inputs"].get("cutoff").is_none());
    // /osc.freq reverted to the operator default rather than dangling on a dead address.
    assert!(after["nodes"][0]["inputs"].get("freq").is_none());
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

/// The precondition a verb reports must be the one that actually failed: renaming a node that
/// isn't there says so, rather than reporting the destination's collision for an edit that could
/// never have run.
#[test]
fn rename_node_reports_the_precondition_that_failed() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_pipe());

    let missing_source = rename_instrument_node(SRC, "/ghost", "/osc", &registry, &resolver)
        .expect_err("no such node");
    assert!(
        missing_source.to_string().contains("/ghost"),
        "the absent source is the failure, not the taken destination: {missing_source}"
    );

    // The interface's input pipes mint into the same address namespace, so a collision there is
    // the same precondition — named as the pipe it is, not left to the loader.
    let minted =
        rename_instrument_node(SRC, "/osc", "/cutoff", &registry, &resolver).expect_err("minted");
    assert!(
        minted.to_string().contains("interface input `cutoff`"),
        "the minted-address collision names the pipe: {minted}"
    );

    // Renaming to the address it already has is a no-op, not a self-collision.
    let noop = rename_instrument_node(SRC, "/osc", "/osc", &registry, &resolver).expect("no-op");
    assert!(noop.report.ok && noop.written);
    assert!(
        noop.notes.iter().any(|n| n.contains("nothing to rename")),
        "the no-op is reported, not miscalled a collision: {:?}",
        noop.notes
    );
}

/// The addresses an OSC host cannot reach, or that read as a wire-ref or a spliced child.
const BAD_ADDRESSES: &[&str] = &["/", "/a b", "//x", "/x/", "/out/sub", "/ünï", "/osc.audio"];

/// The verb's report names the node and nothing is written.
fn assert_refused_as_address(
    result: &EditResult,
    bad: &str,
    before: &serde_json::Value,
    after: &serde_json::Value,
) {
    assert!(
        !result.report.ok,
        "{bad:?} must be refused: {:?}",
        result.report
    );
    assert!(!result.written, "{bad:?}: nothing is written");
    assert_eq!(before, after, "{bad:?}: the document on disk is untouched");
    assert!(
        result
            .report
            .errors
            .iter()
            .any(|d| d.node.as_deref() == Some(bad) && d.message.contains("address")),
        "{bad:?}: the diagnostic names the node and the rule: {:?}",
        result.report.errors
    );
}

#[test]
fn rename_node_to_an_address_breaking_the_rule_is_refused() {
    let registry = Registry::builtin();
    for bad in BAD_ADDRESSES {
        let resolver = resolver_with(&seed());
        let before = readback(&resolver);
        let result = rename_instrument_node(SRC, "/osc", bad, &registry, &resolver)
            .unwrap_or_else(|e| panic!("{bad:?}: the call runs; the edit is refused: {e}"));
        assert_refused_as_address(&result, bad, &before, &readback(&resolver));
    }
    let resolver = resolver_with(&seed());
    let good =
        rename_instrument_node(SRC, "/osc", "/Osc-2_a", &registry, &resolver).expect("rename");
    assert!(good.report.ok && good.written, "{:?}", good.report);
}

#[test]
fn add_node_at_an_address_breaking_the_rule_is_refused() {
    let registry = Registry::builtin();
    let add = |address: &str, resolver: &MemoryResolver| {
        add_instrument_node(
            SRC,
            address,
            "oscillator",
            BTreeMap::new(),
            BTreeMap::new(),
            None,
            None,
            None,
            None,
            &registry,
            resolver,
        )
    };
    for bad in BAD_ADDRESSES {
        let resolver = resolver_with(&seed());
        let before = readback(&resolver);
        let result = add(bad, &resolver)
            .unwrap_or_else(|e| panic!("{bad:?}: the call runs; the edit is refused: {e}"));
        assert_refused_as_address(&result, bad, &before, &readback(&resolver));
    }
    let resolver = resolver_with(&seed());
    let good = add("/lfo_2", &resolver).expect("add");
    assert!(good.report.ok && good.written, "{:?}", good.report);
}

/// Every edit writes the current format, so the edit that migrates a v3 document's refused address
/// is the one that has to report it: the file it leaves behind holds nothing to report again.
#[test]
fn the_edit_that_migrates_a_refused_address_reports_the_rename_once() {
    let registry = Registry::builtin();
    let resolver = resolver_with(
        &json!({
            "format_version": 3,
            "instrument": "t",
            "nodes": [{ "type": "oscillator", "address": "/my osc" }]
        })
        .to_string(),
    );
    let renamed = |result: &EditResult| -> Vec<(Option<String>, String)> {
        result
            .report
            .warnings
            .iter()
            .filter(|d| d.message.contains("renamed"))
            .map(|d| (d.node.clone(), d.message.clone()))
            .collect()
    };

    let first = set_instrument_name(SRC, "u", &registry, &resolver).expect("edit");
    assert!(first.report.ok && first.written, "{:?}", first.report);
    let reported = renamed(&first);
    assert_eq!(reported.len(), 1, "{:?}", first.report.warnings);
    assert_eq!(reported[0].0.as_deref(), Some("/my_osc"));
    assert!(reported[0].1.contains("/my osc"), "{}", reported[0].1);
    assert_eq!(readback(&resolver)["nodes"][0]["address"], json!("/my_osc"));

    let second = set_instrument_name(SRC, "v", &registry, &resolver).expect("edit");
    assert!(second.written);
    assert!(renamed(&second).is_empty(), "{:?}", second.report.warnings);
}

/// Pipe names whose minted `/<name>` an OSC host cannot reach, or that read as a wire-ref or a
/// spliced child.
const BAD_PIPE_NAMES: &[&str] = &["", "a b", "/x", "x/", "out/sub", "ünï", "osc.audio"];

#[test]
fn add_input_pipe_with_a_name_breaking_the_rule_is_refused() {
    let registry = Registry::builtin();
    let add = |name: &str, resolver: &MemoryResolver| {
        add_instrument_interface_input(
            SRC,
            name,
            "f32",
            None,
            Some(json!(0.5)),
            None,
            None,
            None,
            None,
            &registry,
            resolver,
        )
    };
    for bad in BAD_PIPE_NAMES {
        let resolver = resolver_with(&seed());
        let before = readback(&resolver);
        let result = add(bad, &resolver)
            .unwrap_or_else(|e| panic!("{bad:?}: the call runs; the edit is refused: {e}"));
        assert!(!result.report.ok, "{bad:?} must be refused");
        assert!(!result.written, "{bad:?}: nothing is written");
        assert_eq!(
            before,
            readback(&resolver),
            "{bad:?}: the document is untouched"
        );
        assert!(
            result
                .report
                .errors
                .iter()
                .any(|d| d.port.as_deref() == Some(*bad) && d.message.contains("address")),
            "{bad:?}: the diagnostic names the pipe and the rule: {:?}",
            result.report.errors
        );
    }
    let resolver = resolver_with(&seed());
    let good = add("Tone-2_a", &resolver).expect("add");
    assert!(good.report.ok && good.written, "{:?}", good.report);
}

/// As with a node address: the edit that writes a v3 document's renamed pipe reports it, once.
#[test]
fn the_edit_that_migrates_a_refused_pipe_name_reports_the_rename_once() {
    let registry = Registry::builtin();
    let resolver = resolver_with(
        &json!({
            "format_version": 3,
            "instrument": "t",
            "interface": { "inputs": { "my tone": { "type": "f32_buffer", "default": 330.0 } } },
            "nodes": [{ "type": "oscillator", "address": "/osc",
                        "inputs": { "freq": { "from": "/my tone" } } }]
        })
        .to_string(),
    );
    let renamed = |result: &EditResult| -> Vec<(Option<String>, String)> {
        result
            .report
            .warnings
            .iter()
            .filter(|d| d.message.contains("renamed"))
            .map(|d| (d.port.clone(), d.message.clone()))
            .collect()
    };

    let first = set_instrument_name(SRC, "u", &registry, &resolver).expect("edit");
    assert!(first.report.ok && first.written, "{:?}", first.report);
    let reported = renamed(&first);
    assert_eq!(reported.len(), 1, "{:?}", first.report.warnings);
    assert_eq!(reported[0].0.as_deref(), Some("my_tone"));
    assert!(reported[0].1.contains("my tone"), "{}", reported[0].1);
    let after = readback(&resolver);
    assert!(
        after["interface"]["inputs"].get("my_tone").is_some(),
        "{after}"
    );
    assert_eq!(
        after["nodes"][0]["inputs"]["freq"]["from"],
        json!("/my_tone")
    );

    let second = set_instrument_name(SRC, "v", &registry, &resolver).expect("edit");
    assert!(second.written);
    assert!(renamed(&second).is_empty(), "{:?}", second.report.warnings);
}

/// The structure read shows a migrated child's pipe under its new name, so wiring that name looks
/// like wiring an unset port. When the host already sets the pipe under the old name, the edit is
/// refused, naming both, and nothing is written.
#[test]
fn wiring_a_renamed_child_pipe_the_host_already_sets_by_its_old_name_is_refused() {
    let registry = Registry::builtin();
    let mut resolver = resolver_with(
        &json!({
            "format_version": 4,
            "instrument": "h",
            "resources": { "v": "v.json" },
            "interface": { "inputs": {
                "t": { "type": "f32", "default": 0.1 },
                "u": { "type": "f32", "default": 0.2 }
            } },
            "nodes": [{ "type": "subpatch", "address": "/sub", "patch": "v",
                        "inputs": { "a b": { "from": "/t" } } }]
        })
        .to_string(),
    );
    resolver.insert_text(
        "v.json",
        json!({
            "format_version": 3,
            "instrument": "c",
            "interface": { "inputs": {
                "a b": { "type": "f32", "default": 0.5 },
                "a_b": { "type": "f32", "default": 0.5 }
            } },
            "nodes": []
        })
        .to_string(),
    );
    let before = readback(&resolver);
    let result = wire_instrument_input(SRC, "/sub", "a_b_2", "/u", &registry, &resolver)
        .expect("the verb runs; the edit is refused");
    assert!(!result.report.ok && !result.written, "{:?}", result.report);
    assert_eq!(before, readback(&resolver), "nothing is written");
    let error = result
        .report
        .errors
        .iter()
        .find(|d| d.node.as_deref() == Some("/sub") && d.port.as_deref() == Some("a b"))
        .unwrap_or_else(|| panic!("{:?}", result.report.errors));
    assert!(
        error.message.contains(r#"Keep only "a_b_2""#),
        "{}",
        error.message
    );
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
    // The minimal required document and nothing else
    assert_eq!(
        readback(&resolver),
        json!({ "format_version": FORMAT_VERSION, "instrument": "fresh", "nodes": [] }),
        "new_instrument writes exactly the minimal valid document"
    );

    // A second new at the same source refuses rather than clobbering.
    let err = new_instrument(SRC, "other", &registry, &resolver).expect_err("no overwrite");
    assert!(matches!(err, EditError::Target(_)), "got {err:?}");
}

// --- copy_instrument -----------------------------------------------------------------------------

/// A path-shaped store over memory: a key is `dir/name`, and a reference resolves beside the
/// document that makes it. The identity-keyed [`MemoryResolver`] has no directories at all, so it
/// cannot show the one refusal that depends on them.
struct Foldered(MemoryResolver);

impl Foldered {
    /// The shipped kick voice and the one document it nests, as `voices/` siblings.
    fn voices() -> Self {
        let mut texts = MemoryResolver::new();
        texts.insert_text(
            "voices/kick-voice.json",
            include_str!("../../../../instruments/voices/kick-voice.json"),
        );
        texts.insert_text(
            "voices/shaped-vca.json",
            include_str!("../../../../instruments/voices/shaped-vca.json"),
        );
        Foldered(texts)
    }
}

impl ResourceResolver for Foldered {
    fn resolve(&self, source: &str) -> Result<reuben_core::resources::SampleBuffer, ResolveError> {
        self.0.resolve(source)
    }

    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        self.0.resolve_text(source)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.0.write_text(source, text)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        match referrer.and_then(|r| r.rsplit_once('/')) {
            Some((dir, _)) => format!("{dir}/{source}"),
            None => source.to_string(),
        }
    }
}

const KICK: &str = "voices/kick-voice.json";

fn hash_at(resolver: &dyn ResourceResolver, source: &str) -> String {
    let json = resolver.resolve_text(source).expect("read");
    content_hash(
        &NormalizedDoc::from_json(&json, &Registry::builtin(), Some(resolver)).expect("mint"),
    )
}

#[test]
fn copy_writes_a_valid_document_beside_the_original_with_its_hash() {
    let registry = Registry::builtin();
    let resolver = Foldered::voices();
    let original = resolver.resolve_text(KICK).unwrap();

    let result =
        copy_instrument(KICK, "voices/kick-copy.json", &registry, &resolver).expect("copy");

    assert!(result.written, "{:?}", result.report);
    // Warning-clean is what shows the copy was validated where it landed: read from the store's
    // top level instead, its `shaped-vca.json` reference names a key nothing is stored under.
    assert!(
        result.report.ok && result.report.warnings.is_empty(),
        "{:?}",
        result.report
    );
    assert_eq!(result.hash, hash_at(&resolver, KICK));
    assert_eq!(result.hash, hash_at(&resolver, "voices/kick-copy.json"));
    assert_eq!(resolver.resolve_text(KICK).unwrap(), original);
}

#[test]
fn copy_refuses_an_existing_destination_and_leaves_it_alone() {
    let registry = Registry::builtin();
    let resolver = Foldered::voices();
    let taken = resolver.resolve_text("voices/shaped-vca.json").unwrap();

    let err = copy_instrument(KICK, "voices/shaped-vca.json", &registry, &resolver)
        .expect_err("no overwrite");

    assert!(matches!(err, EditError::Target(_)), "got {err:?}");
    assert_eq!(
        resolver.resolve_text("voices/shaped-vca.json").unwrap(),
        taken
    );
}

#[test]
fn copy_refuses_a_destination_in_another_directory() {
    let registry = Registry::builtin();
    let resolver = Foldered::voices();

    for elsewhere in [
        "kits/kick-copy.json",
        "voices/deep/kick-copy.json",
        "kick-copy.json",
    ] {
        let err = copy_instrument(KICK, elsewhere, &registry, &resolver)
            .expect_err("its references would resolve somewhere else");
        assert!(matches!(err, EditError::Target(_)), "got {err:?}");
        assert!(
            resolver.resolve_text(elsewhere).is_err(),
            "{elsewhere} was written"
        );
    }
}

/// A store whose sources are exact keys has no directories: a reference means the same thing
/// from every document, so there is no destination a copy's references could break at.
#[test]
fn copy_between_any_two_keys_of_a_store_without_directories() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());

    let result = copy_instrument(SRC, "elsewhere/copy.json", &registry, &resolver).expect("copy");

    assert!(result.written, "{:?}", result.report);
    assert_eq!(result.hash, hash_at(&resolver, SRC));
}

#[test]
fn copy_of_a_document_that_fails_validation_writes_nothing() {
    let registry = Registry::builtin();
    // Parses and normalizes, but names an operator the registry does not hold.
    let resolver = resolver_with(&seed().replace("oscillator", "nosuchoperator"));

    let result = copy_instrument(SRC, "copy.json", &registry, &resolver)
        .expect("the copy ran; its report is what says no");

    assert!(!result.report.ok && !result.written, "{:?}", result.report);
    assert!(
        resolver.resolve_text("copy.json").is_err(),
        "an invalid copy was written"
    );
}

#[test]
fn copy_of_a_source_that_is_not_a_document_writes_nothing() {
    let registry = Registry::builtin();
    let resolver = resolver_with("{ not json");

    let err = copy_instrument(SRC, "copy.json", &registry, &resolver).expect_err("unparseable");
    assert!(matches!(err, EditError::Parse(_)), "got {err:?}");
    let err = copy_instrument("absent.json", "copy.json", &registry, &resolver)
        .expect_err("nothing to copy");
    assert!(matches!(err, EditError::Read(_)), "got {err:?}");
    assert!(resolver.resolve_text("copy.json").is_err());
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
        Some(50.0),
        None,
        None,
        None,
        &registry,
        &resolver,
    )
    .expect("set meta");
    assert!(meta.written);
    let after = readback(&resolver);
    assert_eq!(after["interface"]["inputs"]["cutoff"]["min"], json!(50.0));
    // The meta verb is the quantity contract and nothing else: the seed is untouched, because it
    // is the value verb's to write.
    assert_eq!(
        after["interface"]["inputs"]["cutoff"]["default"],
        json!(1000.0)
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

#[test]
fn resource_repoint_keeps_the_id_and_refuses_one_that_is_not_there() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());

    add_instrument_resource(SRC, "kick", "kick.wav", &registry, &resolver).expect("add res");
    set_instrument_resource(SRC, "kick", "kick-2.wav", &registry, &resolver).expect("repoint");
    assert_eq!(
        readback(&resolver)["resources"]["kick"],
        json!("kick-2.wav")
    );

    let before = readback(&resolver);
    let refused = set_instrument_resource(SRC, "snare", "snare.wav", &registry, &resolver)
        .expect_err("no such resource");
    assert!(refused.to_string().contains("snare"), "{refused}");
    assert_eq!(readback(&resolver), before);
}

/// [`seed`] with a sample player reading the `kick` resource, both samples in the store.
fn sampled() -> MemoryResolver {
    let doc = json!({
        "format_version": 3,
        "instrument": "test",
        "resources": { "kick": "kick.wav", "snare": "snare.wav" },
        "nodes": [ { "type": "sample", "address": "/hit", "sample": "kick" } ],
        "interface": { "outputs": { "main": { "from": "/hit.audio" } } }
    });
    let mut resolver = resolver_with(&doc.to_string());
    for key in ["kick.wav", "snare.wav", "clap.wav"] {
        resolver.insert_sample(
            key,
            reuben_core::resources::SampleBuffer::new(vec![vec![0.5, -0.5]], 48_000.0),
        );
    }
    resolver
}

#[test]
fn repointing_a_resource_keeps_the_nodes_that_read_it_lit() {
    let registry = Registry::builtin();
    let resolver = sampled();

    let repointed =
        set_instrument_resource(SRC, "kick", "clap.wav", &registry, &resolver).expect("it exists");
    assert!(repointed.written, "{:?}", repointed.report);
    assert!(
        repointed.report.warnings.is_empty(),
        "{:?}",
        repointed.report
    );
    let after = readback(&resolver);
    assert_eq!(after["resources"]["kick"], json!("clap.wav"));
    assert_eq!(after["nodes"][0]["sample"], json!("kick"));

    // Removed instead, the id leaves the node reading nothing: written, and dark.
    let removed = remove_instrument_resource(SRC, "kick", &registry, &resolver).expect("it exists");
    assert!(removed.written);
    assert!(
        (removed.report.warnings.iter()).any(|w| w.node.as_deref() == Some("/hit")),
        "{:?}",
        removed.report
    );
}

#[test]
fn a_node_reads_another_resource_through_its_slot() {
    let registry = Registry::builtin();
    let resolver = sampled();

    let set = set_instrument_node_resource(SRC, "/hit", "sample", "snare", &registry, &resolver)
        .expect("the node and the id exist");
    assert!(set.written, "{:?}", set.report);
    assert_eq!(readback(&resolver)["nodes"][0]["sample"], json!("snare"));

    let removed = remove_instrument_resource(SRC, "kick", &registry, &resolver).expect("it exists");
    assert!(removed.written, "nothing reads `kick` now");
}

#[test]
fn a_subpatch_reads_another_document_through_its_patch_slot() {
    let registry = Registry::builtin();
    let child = |freq: f64| {
        json!({
            "format_version": 3,
            "instrument": "child",
            "nodes": [ { "type": "oscillator", "address": "/osc", "inputs": { "freq": freq } } ],
            "interface": { "outputs": { "out": { "from": "/osc.audio" } } }
        })
        .to_string()
    };
    let parent = json!({
        "format_version": 3,
        "instrument": "test",
        "resources": { "low": "low.json", "high": "high.json", "kick": "kick.wav" },
        "nodes": [ { "type": "subpatch", "address": "/p", "patch": "low" } ],
        "interface": { "outputs": { "main": { "from": "/p.out" } } }
    });
    let mut resolver = resolver_with(&parent.to_string());
    resolver.insert_text("low.json", child(110.0));
    resolver.insert_text("high.json", child(880.0));

    let set = set_instrument_node_resource(SRC, "/p", "patch", "high", &registry, &resolver)
        .expect("the node and the id exist");
    assert!(set.written, "{:?}", set.report);
    assert!(set.report.warnings.is_empty(), "{:?}", set.report);
    assert_eq!(readback(&resolver)["nodes"][0]["patch"], json!("high"));

    // An id naming no document leaves the subpatch dark, as a missing patch does at load: written,
    // with the warning on the node.
    let dark = set_instrument_node_resource(SRC, "/p", "patch", "kick", &registry, &resolver)
        .expect("the id exists");
    assert!(dark.written);
    assert!(
        (dark.report.warnings.iter()).any(|w| w.node.as_deref() == Some("/p")),
        "{:?}",
        dark.report
    );
}

#[test]
fn a_node_resource_names_an_id_the_table_has_and_a_slot_that_exists() {
    let registry = Registry::builtin();
    let resolver = sampled();
    let before = readback(&resolver);
    for (address, slot, id, named) in [
        ("/hit", "sample", "ghost", "ghost"),
        ("/hit", "track", "snare", "track"),
        ("/ghost", "sample", "snare", "ghost"),
    ] {
        let refused = set_instrument_node_resource(SRC, address, slot, id, &registry, &resolver);
        assert!(
            matches!(&refused, Err(EditError::Target(why)) if why.contains(named)),
            "{refused:?}"
        );
    }
    // A slot the node's type does not read is the validation's to reject.
    let wrong = set_instrument_node_resource(SRC, "/hit", "patch", "snare", &registry, &resolver)
        .expect("the slot is a resource slot");
    assert!(!wrong.written);
    assert_eq!(readback(&resolver), before);
}

// --- the intent verb -----------------------------------------------------------------------------

/// A document with everything the intent verb has to navigate: two nodes of one type (broadcast),
/// two more sharing one interface input pipe (the dedupe), an input fed by a real modulation source
/// (the skip), an input with nothing written on it at all (the descriptor default), and an enum
/// port (the `set`).
fn seed_for_intent() -> String {
    json!({
        "format_version": 3,
        "instrument": "intent-test",
        "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": 220.0 } },
            { "type": "filter", "address": "/filter",
              "inputs": { "audio": { "from": "/osc" }, "cutoff": { "from": "/cutoff" } } },
            { "type": "filter", "address": "/mod_filter",
              "inputs": { "audio": { "from": "/osc" }, "cutoff": { "from": "/env.cv" } } },
            { "type": "envelope", "address": "/env" },
            { "type": "snap", "address": "/snap" },
            { "type": "clock", "address": "/clock",
              "inputs": { "tempo": { "from": "/tempo" }, "division": 4.0 } },
            { "type": "clock", "address": "/pad_clock",
              "inputs": { "tempo": { "from": "/tempo" }, "division": 1.0 } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/filter" }, "b": 0.5 } }
        ],
        "interface": {
            "inputs": {
                "cutoff": { "type": "f32_buffer", "min": 200.0, "max": 4000.0,
                            "curve": "lin", "default": 1000.0 },
                "tempo": { "type": "f32", "min": 60.0, "max": 180.0, "default": 130.0, "unit": "BPM" }
            },
            "outputs": { "main": { "from": "/amp" } }
        }
    })
    .to_string()
}

fn by_intent(
    resolver: &MemoryResolver,
    word: &str,
    section: Option<Section>,
    target: &[&str],
) -> EditResult {
    let target: Vec<String> = target.iter().map(|t| t.to_string()).collect();
    set_instrument_inputs_by_intent(SRC, word, section, &target, &Registry::builtin(), resolver)
        .expect("a word in the table is not a refusal")
}

/// The whole lever in one call: a wired input moves the *pipe's* seed against the *pipe's* declared
/// range, an unmatched move is a skip rather than a failure, and a real modulation source is named
/// rather than clobbered.
#[test]
fn an_intent_word_follows_the_wire_and_skips_what_it_cannot_move() {
    let resolver = resolver_with(&seed_for_intent());
    let result = by_intent(&resolver, "warmer", None, &[]);

    assert!(result.written, "{:?}", result.report);
    let after = readback(&resolver);
    // `slightly` on a linear pipe is 10% of the range a human declared: 1000 - 0.10*3800.
    assert_eq!(
        after["interface"]["inputs"]["cutoff"]["default"],
        json!(620.0)
    );
    // The wire itself is untouched — the knob turned, the cable stayed plugged in.
    assert_eq!(
        after["nodes"][1]["inputs"]["cutoff"],
        json!({ "from": "/cutoff" })
    );
    assert!(
        result.zoom.contains("/cutoff.in 1000 → 620"),
        "the echo is the change: {}",
        result.zoom
    );
    // Fed by an envelope, so there is no scalar to move.
    assert!(
        result
            .zoom
            .contains("/mod_filter.cutoff` is wired from `/env.cv`"),
        "{}",
        result.zoom
    );
    // And the two moves this document has no seat for.
    for missing in ["skipped saturator.warmth", "skipped reverb.damp"] {
        assert!(result.zoom.contains(missing), "{}", result.zoom);
    }
    assert!(result
        .zoom
        .starts_with("warmer (timbral): 1 applied, 3 skipped"));
}

/// Two consumers of one pipe are one edit, reported once — the compounding a naive broadcast would
/// do is exactly what makes a batch untrustworthy.
#[test]
fn targets_resolving_to_the_same_pipe_dedupe() {
    let resolver = resolver_with(&seed_for_intent());
    let result = by_intent(&resolver, "faster", None, &[]);

    // Linear 60..180, default step: 130 + 0.25*120.
    assert_eq!(
        readback(&resolver)["interface"]["inputs"]["tempo"]["default"],
        json!(160.0)
    );
    assert!(
        result
            .zoom
            .starts_with("faster (rhythmic): 1 applied, 0 skipped"),
        "{}",
        result.zoom
    );
}

/// A count moves by one, whichever way, and every matching node moves.
#[test]
fn an_integer_port_moves_one_count_per_node() {
    let resolver = resolver_with(&seed_for_intent());
    let result = by_intent(&resolver, "busier", None, &[]);

    let after = readback(&resolver);
    assert_eq!(after["nodes"][5]["inputs"]["division"], json!(5.0));
    assert_eq!(after["nodes"][6]["inputs"]["division"], json!(2.0));
    assert!(
        result.zoom.contains("/clock.division 4 → 5"),
        "{}",
        result.zoom
    );
    assert!(
        result.zoom.contains("skipped euclid.pulses"),
        "{}",
        result.zoom
    );
}

/// An input the document never wrote still has a value: the operator's own declared default. And an
/// exponential port moves by a ratio, so its range never enters the arithmetic.
#[test]
fn an_unwritten_input_moves_from_its_descriptor_default() {
    let resolver = resolver_with(&seed_for_intent());
    let result = by_intent(&resolver, "softer", None, &[]);

    // `envelope.attack` is exponential, default 0.01, default step ×1.5.
    assert_eq!(
        readback(&resolver)["nodes"][3]["inputs"]["attack"],
        json!(0.015)
    );
    assert!(
        result.zoom.contains("/env.attack 0.01 → 0.015"),
        "{}",
        result.zoom
    );
}

/// A third of the table assigns rather than shoves, and an enum is assigned by symbol.
#[test]
fn a_set_move_assigns_an_enum_symbol() {
    let resolver = resolver_with(&seed_for_intent());
    let result = by_intent(&resolver, "more consonant", None, &[]);

    assert_eq!(
        readback(&resolver)["nodes"][4]["inputs"]["target"],
        json!("Chord")
    );
    assert!(
        result
            .zoom
            .contains("/snap.target Scale → Chord [snap to chord tones]"),
        "the curated hedge arrives after the act: {}",
        result.zoom
    );
}

/// `target` is the projection's selection grammar, and a term that named nothing is reported rather
/// than silently dropped.
#[test]
fn target_narrows_and_reports_what_it_did_not_match() {
    let resolver = resolver_with(&seed_for_intent());
    let result = by_intent(&resolver, "warmer", None, &["/mod_filter", "/nope"]);

    // `/filter` was outside the target, so its pipe is untouched.
    assert_eq!(
        readback(&resolver)["interface"]["inputs"]["cutoff"]["default"],
        json!(1000.0)
    );
    assert!(result.zoom.contains("unmatched: /nope"), "{}", result.zoom);
}

/// The one overloaded word: the most likely reading is applied, and the report names the reading it
/// passed over — the table's own preamble, mechanized.
#[test]
fn an_overloaded_word_takes_a_reading_and_names_the_other() {
    let resolver = resolver_with(&seed_for_intent());
    let timbral = by_intent(&resolver, "darker", None, &[]);
    assert!(
        timbral.zoom.starts_with("darker (timbral)"),
        "{}",
        timbral.zoom
    );
    assert!(
        timbral
            .zoom
            .contains("also a tonal word — pass section \"tonal\""),
        "{}",
        timbral.zoom
    );

    let tonal = by_intent(&resolver, "darker", Some(Section::Tonal), &[]);
    assert!(tonal.zoom.starts_with("darker (tonal)"), "{}", tonal.zoom);
    assert!(
        !tonal.zoom.contains("also a"),
        "nothing was passed over: {}",
        tonal.zoom
    );
}

/// A word the table does not carry is a precondition failure, not a batch of zero edits: there is
/// nothing to report, and a `written: true` for it would be a lie.
#[test]
fn a_word_outside_the_table_is_a_precondition_error() {
    let resolver = resolver_with(&seed_for_intent());
    let err =
        set_instrument_inputs_by_intent(SRC, "shinier", None, &[], &Registry::builtin(), &resolver)
            .expect_err("not a word in the table");
    assert!(err.to_string().contains("shinier"), "{err}");
}

/// A pipe-fed graph shaped by its interface block: one instrument, one wired consumer per pipe, so
/// each test declares exactly the pipe contract it is about.
fn seed_piped(pipes: serde_json::Value, nodes: serde_json::Value) -> String {
    json!({
        "format_version": 3,
        "instrument": "piped",
        "nodes": nodes,
        "interface": { "inputs": pipes, "outputs": { "main": { "from": "/amp" } } }
    })
    .to_string()
}

/// A pipe that declares no range has no range: the loader fills the gap with the type-wide ±1e6
/// sentinel, and a quarter of *that* is half a million written into the document. The contract falls
/// back to the port the pipe feeds, which is the only side that knows the musical span.
#[test]
fn a_pipe_with_no_declared_range_is_sized_by_the_port_it_feeds() {
    let resolver = resolver_with(&seed_piped(
        json!({ "wet": { "type": "f32", "default": 0.3 } }),
        json!([
            { "type": "oscillator", "address": "/osc" },
            { "type": "reverb", "address": "/rv",
              "inputs": { "audio": { "from": "/osc" }, "mix": { "from": "/wet" } } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/rv" }, "b": 0.5 } }
        ]),
    ));
    let result = by_intent(&resolver, "wetter", None, &[]);

    // `reverb.mix` is 0..1 linear, so the default step is a quarter of one — not of two million.
    assert_eq!(
        readback(&resolver)["interface"]["inputs"]["wet"]["default"],
        json!(0.55)
    );
    assert!(
        result.zoom.contains("/wet.in 0.3 → 0.55"),
        "{}",
        result.zoom
    );
}

/// A non-finite bound must never reach the arithmetic. JSON cannot spell an infinity, so serde
/// writes one as `null` — the seed would be *erased* while the report claimed the move landed, and
/// a null seed is legal so nothing downstream would catch it.
#[test]
fn a_non_finite_range_is_skipped_rather_than_written() {
    let resolver = resolver_with(&seed_piped(
        // `1e40` is finite as an `f64` and infinite as the `f32` the pipe contract is built in.
        json!({ "cut": { "type": "f32", "min": 20.0, "max": 1e40, "curve": "lin",
                         "default": 1000.0 } }),
        json!([
            { "type": "oscillator", "address": "/osc" },
            { "type": "filter", "address": "/f",
              "inputs": { "audio": { "from": "/osc" }, "cutoff": { "from": "/cut" } } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/f" }, "b": 0.5 } }
        ]),
    ));
    let result = by_intent(&resolver, "brighter", None, &[]);

    let seed = &readback(&resolver)["interface"]["inputs"]["cut"]["default"];
    assert_eq!(seed, &json!(1000.0), "the seed survives: {seed}");
    assert!(
        result.zoom.contains("not a finite value"),
        "the skip says why: {}",
        result.zoom
    );
    assert!(result.zoom.starts_with("brighter (timbral): 0 applied"));
}

/// A move that lands where it started is not a move. The report is the only account of the edit
/// there is, so "applied" over a byte-identical document is the one thing it cannot say.
#[test]
fn a_move_that_clamps_to_where_it_started_is_a_skip() {
    let resolver = resolver_with(
        &json!({
            "format_version": 3,
            "instrument": "at-the-edge",
            "nodes": [
                { "type": "oscillator", "address": "/osc" },
                { "type": "filter", "address": "/f",
                  "inputs": { "audio": { "from": "/osc" }, "cutoff": 20000.0 } },
                { "type": "reverb", "address": "/rv",
                  "inputs": { "audio": { "from": "/f" }, "mix": 1.0, "damp": 0.0 } },
                { "type": "mul_f32_signal", "address": "/amp",
                  "inputs": { "a": { "from": "/rv" }, "b": 0.5 } }
            ],
            "interface": { "outputs": { "main": { "from": "/amp" } } }
        })
        .to_string(),
    );
    let result = by_intent(&resolver, "airier", None, &[]);

    assert!(
        result
            .zoom
            .starts_with("airier (timbral): 0 applied, 3 skipped"),
        "{}",
        result.zoom
    );
    assert!(result.zoom.contains("is already 1"), "{}", result.zoom);
    let after = readback(&resolver);
    assert_eq!(after["nodes"][2]["inputs"]["mix"], json!(1.0));
    assert_eq!(after["nodes"][2]["inputs"]["damp"], json!(0.0));
    assert_eq!(after["nodes"][1]["inputs"]["cutoff"], json!(20000.0));
}

/// Omitting `curve` is not an assertion of linearity — the loader's default is, and taking it would
/// put a fraction-of-range step on a port whose response is a ratio.
#[test]
fn a_pipe_with_no_declared_curve_inherits_the_ports_response() {
    let resolver = resolver_with(&seed_piped(
        json!({ "glide": { "type": "f32", "min": 0.0, "max": 0.5, "default": 0.06 } }),
        json!([
            { "type": "oscillator", "address": "/osc" },
            { "type": "m2s", "address": "/m", "inputs": { "time": { "from": "/glide" } } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/osc" }, "b": 0.5 } }
        ]),
    ));
    let result = by_intent(&resolver, "looser", None, &[]);

    // `m2s.time` is exponential: ×1.5, not a quarter of the pipe's own 0..0.5 span (which would be
    // 0.185 — a 185 ms glide where the port's own geometry says 90.
    assert_eq!(
        readback(&resolver)["interface"]["inputs"]["glide"]["default"],
        json!(0.09)
    );
    assert!(
        result.zoom.contains("/glide.in 0.06 → 0.09"),
        "{}",
        result.zoom
    );
}

/// A channel-bound pipe still moves — its seed is what a nested or unfed instrument plays — but the
/// caller never named it, the wire led here, so the note says when the move is inaudible.
#[test]
fn moving_a_channel_bound_pipe_is_noted() {
    let resolver = resolver_with(&seed_piped(
        json!({ "cut": { "type": "f32_buffer", "channel": 0, "min": 200.0, "max": 4000.0,
                         "curve": "lin", "default": 1000.0 } }),
        json!([
            { "type": "oscillator", "address": "/osc" },
            { "type": "filter", "address": "/f",
              "inputs": { "audio": { "from": "/osc" }, "cutoff": { "from": "/cut" } } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/f" }, "b": 0.5 } }
        ]),
    ));
    let result = by_intent(&resolver, "warmer", None, &[]);

    assert!(
        result.zoom.contains("/cut.in 1000 → 620"),
        "{}",
        result.zoom
    );
    assert!(
        result.notes.iter().any(|n| n.contains("input channel 0")),
        "the caveat rides the edit: {:?}",
        result.notes
    );
}

/// `target` takes the addresses the report hands back. The wire-followed address is a pipe's, so
/// narrowing by the address just echoed has to reach the same slot — one address space or none.
#[test]
fn target_accepts_the_pipe_address_the_report_echoes() {
    let seed = seed_piped(
        json!({ "cut": { "type": "f32", "min": 200.0, "max": 4000.0, "curve": "lin",
                         "default": 1000.0 } }),
        json!([
            { "type": "oscillator", "address": "/osc" },
            { "type": "filter", "address": "/f",
              "inputs": { "audio": { "from": "/osc" }, "cutoff": { "from": "/cut" } } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/f" }, "b": 0.5 } }
        ]),
    );
    let resolver = resolver_with(&seed);
    let broadcast = by_intent(&resolver, "warmer", None, &[]);
    assert!(broadcast.zoom.contains("/cut.in"), "{}", broadcast.zoom);

    // The same word again, narrowed by the address that echo just named.
    let resolver = resolver_with(&seed);
    let narrowed = by_intent(&resolver, "warmer", None, &["/cut"]);
    assert_eq!(
        readback(&resolver)["interface"]["inputs"]["cut"]["default"],
        json!(620.0)
    );
    assert!(
        !narrowed.zoom.contains("unmatched"),
        "the echoed address matched: {}",
        narrowed.zoom
    );
}

/// The dedupe is on a slot that was *written*. A slot a first move could not move is still a slot a
/// second move can — the two arrive with different contracts, because they are fed by different
/// ports — so blocking it on the first attempt loses the second entirely.
#[test]
fn a_slot_a_move_could_not_write_stays_open_to_the_next_move() {
    let resolver = resolver_with(&seed_piped(
        json!({ "p": { "type": "f32", "default": 20000.0 } }),
        json!([
            { "type": "oscillator", "address": "/osc" },
            { "type": "filter", "address": "/f",
              "inputs": { "audio": { "from": "/osc" }, "cutoff": { "from": "/p" },
                          "resonance": { "from": "/p" } } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/f" }, "b": 0.5 } }
        ]),
    ));
    // `harsher` is saturator.drive up; filter.cutoff up; filter.resonance up. Against `cutoff`'s
    // 20..20000 the seed is already at the ceiling — a skip; against `resonance`'s 0..1 it is not.
    let result = by_intent(&resolver, "harsher", None, &[]);

    assert!(
        result.zoom.contains("/p.in 20000 → 1"),
        "the second move still reached the slot: {}",
        result.zoom
    );
    assert!(
        result
            .zoom
            .starts_with("harsher (timbral): 1 applied, 2 skipped"),
        "{}",
        result.zoom
    );
}

/// A `set` names a specific thing — a minor 3rd, a rotation of 0 — so a range that will not hold it
/// has no smaller version of it to offer. Clamping would land a *different* interval and the row's
/// own description would then label it: `s2 → 5` reported as "a minor 3rd" while being a 4th.
#[test]
fn a_set_the_range_cannot_hold_is_a_skip_not_a_clamp() {
    let resolver = resolver_with(&seed_piped(
        json!({ "iv": { "type": "i32", "min": 5.0, "max": 9.0, "default": 7.0 } }),
        json!([
            { "type": "oscillator", "address": "/osc" },
            { "type": "harmony", "address": "/h", "inputs": { "s2": { "from": "/iv" } } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/osc" }, "b": 0.5 } }
        ]),
    ));
    // `sadder` sets s2 to a minor 3rd, s5 to a minor 6th, s6 to a minor 7th. Only s2 is pinned
    // behind a range that excludes the interval it names.
    let result = by_intent(&resolver, "sadder", None, &[]);

    let after = readback(&resolver);
    assert_eq!(
        after["interface"]["inputs"]["iv"]["default"],
        json!(7.0),
        "an unreachable interval leaves the value alone"
    );
    assert!(
        result.zoom.contains("cannot be set to 3"),
        "the skip names the value it could not reach: {}",
        result.zoom
    );
    // The siblings it *can* reach still land — a batch is not all-or-nothing.
    assert_eq!(after["nodes"][1]["inputs"]["s5"], json!(8.0));
    assert_eq!(after["nodes"][1]["inputs"]["s6"], json!(10.0));
    assert!(result
        .zoom
        .starts_with("sadder (tonal): 2 applied, 1 skipped"));
}

/// One slot reached through three consumers is one problem. The successful path already collapses
/// those three to one applied line; three copies of one skip read as three distinct failures.
#[test]
fn skips_that_name_the_same_slot_and_reason_collapse() {
    let resolver = resolver_with(&seed_piped(
        json!({ "tempo": { "type": "f32", "min": 60.0, "max": 180.0, "curve": "lin",
                           "default": 180.0 } }),
        json!([
            { "type": "oscillator", "address": "/osc" },
            { "type": "clock", "address": "/c1", "inputs": { "tempo": { "from": "/tempo" } } },
            { "type": "clock", "address": "/c2", "inputs": { "tempo": { "from": "/tempo" } } },
            { "type": "clock", "address": "/c3", "inputs": { "tempo": { "from": "/tempo" } } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/osc" }, "b": 0.5 } }
        ]),
    ));
    let result = by_intent(&resolver, "faster", None, &[]);

    assert!(
        result
            .zoom
            .starts_with("faster (rhythmic): 0 applied, 1 skipped"),
        "{}",
        result.zoom
    );
    assert_eq!(
        result.zoom.matches("/tempo.in` is already 180").count(),
        1,
        "{}",
        result.zoom
    );
}

// --- layout ---------------------------------------------------------------------------------------

/// [`seed`] with an input pipe and an output pipe that share the name `main` — legal, because output
/// pipes never enter the node namespace.
fn seed_with_twin_pipes() -> String {
    json!({
        "format_version": 3,
        "instrument": "test",
        "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/main" } } },
            { "type": "mul_f32_signal", "address": "/amp", "inputs": { "a": { "from": "/osc" }, "b": 0.5 } }
        ],
        "interface": {
            "inputs": { "main": { "type": "f32", "default": 220.0 } },
            "outputs": { "main": { "from": "/amp" } }
        }
    })
    .to_string()
}

fn at(x: f32, y: f32) -> Layout {
    Layout { x, y }
}

fn node(address: &str) -> LayoutTarget {
    LayoutTarget::Node(address.to_string())
}

fn output(name: &str) -> LayoutTarget {
    LayoutTarget::Output(name.to_string())
}

#[test]
fn set_layout_places_nodes_input_pipes_and_output_pipes_in_one_write() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_twin_pipes());

    let result = set_instrument_layout(
        SRC,
        &[
            (node("/osc"), at(10.0, 20.0)),
            (node("/amp"), at(200.5, -4.25)),
            (node("/main"), at(-80.0, 20.0)),
            (output("main"), at(400.0, 0.0)),
        ],
        &registry,
        &resolver,
    )
    .expect("every target exists");

    assert!(result.written, "{:?}", result.report);
    let after = readback(&resolver);
    assert_eq!(after["nodes"][0]["layout"], json!({ "x": 10.0, "y": 20.0 }));
    assert_eq!(
        after["nodes"][1]["layout"],
        json!({ "x": 200.5, "y": -4.25 })
    );
    // The twin names land on their own sides: `/main` is the input pipe, `{output: main}` the tap.
    assert_eq!(
        after["interface"]["inputs"]["main"]["layout"],
        json!({ "x": -80.0, "y": 20.0 })
    );
    assert_eq!(
        after["interface"]["outputs"]["main"]["layout"],
        json!({ "x": 400.0, "y": 0.0 })
    );
}

#[test]
fn set_layout_leaves_unnamed_targets_where_they_were() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_twin_pipes());
    set_instrument_layout(SRC, &[(node("/osc"), at(1.0, 2.0))], &registry, &resolver)
        .expect("first placement");
    set_instrument_layout(SRC, &[(node("/amp"), at(3.0, 4.0))], &registry, &resolver)
        .expect("second placement");

    let after = readback(&resolver);
    assert_eq!(after["nodes"][0]["layout"], json!({ "x": 1.0, "y": 2.0 }));
    assert_eq!(after["nodes"][1]["layout"], json!({ "x": 3.0, "y": 4.0 }));
    assert!(after["interface"]["outputs"]["main"]
        .get("layout")
        .is_none());
}

#[test]
fn set_layout_moves_the_content_hash() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let before = set_instrument_description(SRC, None, &registry, &resolver)
        .expect("a no-op write to read the hash")
        .hash;
    let after = set_instrument_layout(SRC, &[(node("/osc"), at(5.0, 5.0))], &registry, &resolver)
        .expect("placement")
        .hash;
    assert_ne!(before, after, "a layout-only edit is still an edit");
}

#[test]
fn set_layout_rejects_the_whole_call_on_an_unknown_target_of_either_kind() {
    let registry = Registry::builtin();
    for (ghost, named) in [
        (node("/ghost"), "/ghost"),
        (output("ghost"), "ghost"),
        // Each side's names are its own: the output pipe `main` is not the node `/main`, and the
        // input pipe `cutoff` is not an output pipe.
        (node("/main"), "/main"),
        (output("cutoff"), "cutoff"),
    ] {
        let resolver = resolver_with(&seed_with_pipe());
        let before = resolver.resolve_text(SRC).expect("seed");
        let err = set_instrument_layout(
            SRC,
            &[(node("/osc"), at(1.0, 1.0)), (ghost, at(2.0, 2.0))],
            &registry,
            &resolver,
        )
        .expect_err("an unknown target refuses");
        assert!(matches!(err, EditError::Target(_)), "got {err:?}");
        assert!(err.to_string().contains(named), "{err}");
        assert_eq!(
            resolver.resolve_text(SRC).expect("still there"),
            before,
            "nothing written — not even the target that did exist"
        );
    }
}

#[test]
fn set_layout_refuses_a_target_named_twice() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let err = set_instrument_layout(
        SRC,
        &[(node("/osc"), at(1.0, 1.0)), (node("/osc"), at(2.0, 2.0))],
        &registry,
        &resolver,
    )
    .expect_err("which position is meant?");
    assert!(err.to_string().contains("/osc"), "{err}");
}

#[test]
fn set_layout_refuses_a_coordinate_that_would_not_reload() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    for bad in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
        let err = set_instrument_layout(SRC, &[(node("/osc"), at(bad, 0.0))], &registry, &resolver)
            .expect_err("a non-finite coordinate saves as `null`");
        assert!(matches!(err, EditError::Target(_)), "got {err:?}");
    }
}

// --- whole-document restore ---------------------------------------------------------------------

#[test]
fn restore_writes_the_prior_text_canonically_and_reports_its_hash() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let prior = seed();
    let prior_hash = content_hash(
        &NormalizedDoc::from_json(&prior, &registry, Some(&resolver)).expect("the seed loads"),
    );
    let edited = set_instrument_input(SRC, "/osc", "freq", json!(880.0), &registry, &resolver)
        .expect("edit");
    assert_ne!(edited.hash, prior_hash);

    let restored = restore_instrument(SRC, &prior, &prior_hash, &edited.hash, &registry, &resolver)
        .expect("restore")
        .expect("the source is as expected");
    assert!(restored.written, "{restored:?}");
    assert_eq!(restored.hash, prior_hash);
    let on_disk = resolver.resolve_text(SRC).expect("written");
    assert_eq!(
        on_disk,
        NormalizedDoc::from_json(&prior, &registry, Some(&resolver))
            .expect("the seed loads")
            .to_document_text(),
        "the canonical form is written, not the text handed in"
    );
}

#[test]
fn restore_to_a_prior_that_no_longer_normalizes_is_a_rejected_edit() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let before = resolver.resolve_text(SRC).expect("seeded");
    let current_hash = content_hash(
        &NormalizedDoc::from_json(&before, &registry, Some(&resolver)).expect("the seed loads"),
    );
    let from_the_future = json!({ "format_version": FORMAT_VERSION + 1, "instrument": "x",
                                  "nodes": [] })
    .to_string();

    let result = restore_instrument(
        SRC,
        &from_the_future,
        "0000000000000000",
        &current_hash,
        &registry,
        &resolver,
    )
    .expect("the verb ran; the prior is what failed")
    .expect("the source is as expected");
    assert!(!result.written);
    assert!(!result.report.ok);
    assert_eq!(result.report.errors.len(), 1, "{:?}", result.report);
    assert_eq!(result.hash, current_hash);
    assert_eq!(resolver.resolve_text(SRC).expect("still there"), before);
}

#[test]
fn restore_over_a_source_that_does_not_parse_is_refused() {
    let registry = Registry::builtin();
    let resolver = resolver_with("{ not a document");
    let err = restore_instrument(SRC, &seed(), "0", "0", &registry, &resolver)
        .expect_err("nothing persisted has a hash to report");
    assert!(matches!(err, EditError::Parse(_)), "got {err:?}");
    assert_eq!(
        resolver.resolve_text(SRC).expect("still there"),
        "{ not a document"
    );
}

#[test]
fn restore_over_a_source_that_is_not_the_one_expected_does_nothing() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let seeded = resolver.resolve_text(SRC).expect("seeded");
    let prior_hash = content_hash(
        &NormalizedDoc::from_json(&seeded, &registry, Some(&resolver)).expect("the seed loads"),
    );

    let stale = restore_instrument(
        SRC,
        &seeded,
        &prior_hash,
        "not-what-is-there",
        &registry,
        &resolver,
    )
    .expect("the verb ran");

    assert!(stale.is_none());
    assert_eq!(resolver.resolve_text(SRC).expect("still there"), seeded);
}

#[test]
fn restore_of_a_prior_that_is_not_the_document_asked_for_is_a_rejected_edit() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let seeded = resolver.resolve_text(SRC).expect("seeded");
    let edited = set_instrument_input(SRC, "/osc", "freq", json!(880.0), &registry, &resolver)
        .expect("edit");
    let after = resolver.resolve_text(SRC).expect("edited");

    let result = restore_instrument(
        SRC,
        &seeded,
        "0000000000000000",
        &edited.hash,
        &registry,
        &resolver,
    )
    .expect("the verb ran")
    .expect("the source is as expected");

    assert!(!result.written && !result.report.ok, "{result:?}");
    assert_eq!(result.hash, edited.hash);
    assert_eq!(resolver.resolve_text(SRC).expect("still there"), after);
}

// --- written text --------------------------------------------------------------------------------

fn ends_in_one_newline(text: &str) -> bool {
    text.ends_with('\n') && !text.ends_with("\n\n")
}

#[test]
fn every_verb_written_document_ends_in_exactly_one_newline() {
    let registry = Registry::builtin();

    let resolver = resolver_with(&seed());
    let edited = set_instrument_input(SRC, "/osc", "freq", json!(880.0), &registry, &resolver)
        .expect("edit");
    assert!(edited.written, "{edited:?}");
    let text = resolver.resolve_text(SRC).expect("edited");
    assert!(ends_in_one_newline(&text), "an edit: {text:?}");

    let restored = restore_instrument(
        SRC,
        &seed(),
        &content_hash(&NormalizedDoc::from_json(&seed(), &registry, Some(&resolver)).unwrap()),
        &edited.hash,
        &registry,
        &resolver,
    )
    .expect("restore")
    .expect("the source is as expected");
    assert!(restored.written, "{restored:?}");
    let text = resolver.resolve_text(SRC).expect("restored");
    assert!(ends_in_one_newline(&text), "a restore: {text:?}");

    let fresh = MemoryResolver::new();
    new_instrument(SRC, "fresh", &registry, &fresh).expect("new");
    let text = fresh.resolve_text(SRC).expect("created");
    assert!(ends_in_one_newline(&text), "a new document: {text:?}");

    let foldered = Foldered::voices();
    let copied =
        copy_instrument(KICK, "voices/kick-copy.json", &registry, &foldered).expect("copy");
    assert!(copied.written, "{copied:?}");
    let text = foldered
        .resolve_text("voices/kick-copy.json")
        .expect("copied");
    assert!(ends_in_one_newline(&text), "a copy: {text:?}");
}

#[test]
fn the_trailing_newline_is_not_part_of_the_content_hash() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let edited = set_instrument_input(SRC, "/osc", "freq", json!(880.0), &registry, &resolver)
        .expect("edit");
    let text = resolver.resolve_text(SRC).expect("edited");
    let bare = text.trim_end_matches('\n');
    assert_eq!(edited.hash, hash_at(&resolver, SRC));
    let mint = |t: &str| {
        content_hash(&NormalizedDoc::from_json(t, &registry, Some(&resolver)).expect("mint"))
    };
    assert_eq!(mint(&text), mint(bare));
}

// --- output pipe feed ----------------------------------------------------------------------------

#[test]
fn wire_output_feeds_the_pipe_from_another_port_and_keeps_its_metadata() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    set_instrument_interface_output_meta(
        SRC,
        "main",
        None,
        None,
        None,
        Some("V"),
        &registry,
        &resolver,
    )
    .expect("a unit");
    set_instrument_layout(SRC, &[(output("main"), at(9.0, 3.0))], &registry, &resolver)
        .expect("a position");

    let result = wire_instrument_interface_output(SRC, "main", "/osc", &registry, &resolver)
        .expect("the pipe exists");

    assert!(result.written, "{:?}", result.report);
    let main = &readback(&resolver)["interface"]["outputs"]["main"];
    assert_eq!(main["from"], json!("/osc"));
    assert_eq!(main["unit"], json!("V"));
    assert_eq!(main["layout"], json!({ "x": 9.0, "y": 3.0 }));
}

#[test]
fn wire_output_from_a_port_that_does_not_exist_is_rejected_and_not_written() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let before = readback(&resolver);

    let result = wire_instrument_interface_output(SRC, "main", "/ghost", &registry, &resolver)
        .expect("the pipe exists");

    assert!(!result.written);
    assert!(!result.report.ok);
    assert_eq!(readback(&resolver), before);
}

#[test]
fn wire_output_refuses_an_output_pipe_the_document_does_not_have() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed());
    let refused = wire_instrument_interface_output(SRC, "ghost", "/osc", &registry, &resolver);
    assert!(
        matches!(&refused, Err(EditError::Target(why)) if why.contains("ghost")),
        "{refused:?}"
    );
}

// --- the inverse verbs: clearing pipe metadata, removing a constant ------------------------------

/// [`seed_with_pipe`] with the metadata the set-meta verbs write held by a pipe of each direction.
/// `gain` is a signal pipe, the only kind that may bind a `channel`; `main` holds no range, since
/// the loader takes one only on an output fed from a port that declares a numeric range.
fn seed_with_meta() -> String {
    json!({
        "format_version": 3,
        "instrument": "test",
        "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/cutoff" } } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/osc" }, "b": { "from": "/gain" } } }
        ],
        "interface": {
            "inputs": {
                "cutoff": { "type": "f32", "min": 20.0, "max": 20000.0, "curve": "exp",
                            "unit": "Hz", "default": 440.0 },
                "gain": { "type": "f32_buffer", "channel": 0, "min": 0.0, "max": 1.0, "unit": "x" }
            },
            "outputs": { "main": { "from": "/amp", "channel": 0, "unit": "V" } }
        }
    })
    .to_string()
}

#[test]
fn clearing_input_meta_drops_each_named_field_and_keeps_the_rest() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_meta());

    let result = clear_instrument_interface_meta(
        SRC,
        "cutoff",
        PipeDirection::Input,
        &[MetaField::Min, MetaField::Curve, MetaField::Unit],
        &registry,
        &resolver,
    )
    .expect("the pipe holds every field named");
    assert!(result.report.ok && result.written, "{result:?}");
    assert!(result.notes.is_empty(), "{:?}", result.notes);

    let cutoff = &readback(&resolver)["interface"]["inputs"]["cutoff"];
    for gone in ["min", "curve", "unit"] {
        assert!(cutoff.get(gone).is_none(), "`{gone}` is cleared: {cutoff}");
    }
    assert_eq!(cutoff["max"], json!(20000.0), "an unnamed field is kept");
    assert_eq!(
        cutoff["default"],
        json!(440.0),
        "the seed is the value verb's"
    );

    let channel = clear_instrument_interface_meta(
        SRC,
        "gain",
        PipeDirection::Input,
        &[MetaField::Channel, MetaField::Max],
        &registry,
        &resolver,
    )
    .expect("a signal pipe's channel clears");
    assert!(channel.written, "{channel:?}");
    let gain = &readback(&resolver)["interface"]["inputs"]["gain"];
    assert!(
        gain.get("channel").is_none() && gain.get("max").is_none(),
        "{gain}"
    );
    assert_eq!(gain["min"], json!(0.0));
}

#[test]
fn clearing_output_meta_drops_each_named_field_and_keeps_the_feed() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_meta());

    let result = clear_instrument_interface_meta(
        SRC,
        "main",
        PipeDirection::Output,
        &[MetaField::Channel, MetaField::Unit],
        &registry,
        &resolver,
    )
    .expect("the output holds every field named");
    assert!(result.report.ok && result.written, "{result:?}");

    let main = &readback(&resolver)["interface"]["outputs"]["main"];
    assert_eq!(main, &json!({ "from": "/amp" }), "only the feed is left");
}

/// The pipe already ends where it was asked to, so an absent field is the caller's to hear about
/// and nothing to refuse — the posture `unwire_instrument_input` takes on an input with nothing on
/// it. A repeated field is cleared once and noted never.
#[test]
fn clearing_a_field_the_pipe_does_not_hold_is_noted_not_refused() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_pipe());
    let before = readback(&resolver);

    let result = clear_instrument_interface_meta(
        SRC,
        "cutoff",
        PipeDirection::Input,
        &[MetaField::Unit, MetaField::Min, MetaField::Min],
        &registry,
        &resolver,
    )
    .expect("an absent field is not a precondition failure");
    assert!(result.report.ok && result.written, "{result:?}");
    assert_eq!(
        result.notes,
        ["`unit` was not set on input pipe `cutoff`; nothing to clear"]
    );
    let after = readback(&resolver);
    assert!(after["interface"]["inputs"]["cutoff"].get("min").is_none());
    assert_eq!(
        after["interface"]["inputs"]["cutoff"]["max"],
        before["interface"]["inputs"]["cutoff"]["max"]
    );
}

#[test]
fn clearing_meta_refuses_what_no_pipe_could_satisfy_and_writes_nothing() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_meta());
    let before = readback(&resolver);
    let clear = |name: &str, direction, fields: &[MetaField]| {
        clear_instrument_interface_meta(SRC, name, direction, fields, &registry, &resolver)
            .expect_err("refused")
            .to_string()
    };

    let curve = clear(
        "main",
        PipeDirection::Output,
        &[MetaField::Unit, MetaField::Curve],
    );
    assert!(
        curve.contains("`curve`") && curve.contains("output pipe"),
        "a field the direction cannot carry is named: {curve}"
    );
    assert!(clear("cutoff", PipeDirection::Input, &[]).contains("at least one"));
    assert!(clear("ghost", PipeDirection::Input, &[MetaField::Unit])
        .contains("no interface input `ghost`"));
    assert!(
        clear("cutoff", PipeDirection::Output, &[MetaField::Unit])
            .contains("no interface output `cutoff`"),
        "the two directions are separate namespaces"
    );
    assert!(clear("main", PipeDirection::Input, &[MetaField::Unit])
        .contains("no interface input `main`"));

    assert_eq!(before, readback(&resolver), "a refusal writes nothing");
}

#[test]
fn removing_a_constant_reverts_it_to_the_operators_default() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_voicer());
    set_instrument_constant(SRC, "/voices", "voices", json!(4), &registry, &resolver)
        .expect("set constant");
    assert_eq!(
        readback(&resolver)["nodes"][0]["config"]["voices"].as_f64(),
        Some(4.0)
    );

    let removed = remove_instrument_constant(SRC, "/voices", "voices", &registry, &resolver)
        .expect("the constant is set");
    assert!(removed.report.ok && removed.written, "{removed:?}");
    assert!(removed.notes.is_empty(), "{:?}", removed.notes);
    assert!(
        readback(&resolver)["nodes"][0].get("config").is_none(),
        "the node holds no override: {}",
        readback(&resolver)["nodes"][0]
    );
    assert!(removed.zoom.contains("/voices"), "{}", removed.zoom);

    let again = remove_instrument_constant(SRC, "/voices", "voices", &registry, &resolver)
        .expect("a constant that is not set is not a precondition failure");
    assert!(again.written, "{again:?}");
    assert_eq!(again.hash, removed.hash, "nothing changed");
    assert_eq!(
        again.notes,
        ["`voices` was not set on `/voices`; nothing to remove"]
    );
}

/// A name the operator does not declare cannot be "not set": it is a misspelling, and answering it
/// with a note would tell the caller the edit it meant had landed.
#[test]
fn removing_a_constant_the_operator_does_not_declare_is_refused_naming_the_declared_ones() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_voicer());
    let before = readback(&resolver);

    let typo = remove_instrument_constant(SRC, "/voices", "voicez", &registry, &resolver)
        .expect_err("the voicer declares no `voicez`");
    let message = typo.to_string();
    assert!(matches!(typo, EditError::Target(_)), "{typo:?}");
    assert!(
        message.contains("`voicez`") && message.contains("`voices`"),
        "the refusal names the constants there are: {message}"
    );

    let none = remove_instrument_constant(SRC, "/amp", "voices", &registry, &resolver)
        .expect_err("a multiply declares no constants");
    assert!(none.to_string().contains("declares none"), "{none}");
    assert_eq!(before, readback(&resolver));
}

#[test]
fn removing_a_constant_from_no_node_is_refused_and_writes_nothing() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_voicer());
    let before = readback(&resolver);

    let err = remove_instrument_constant(SRC, "/ghost", "voices", &registry, &resolver)
        .expect_err("no such node");
    assert!(matches!(err, EditError::Target(_)), "{err:?}");
    assert!(err.to_string().contains("/ghost"), "{err}");
    assert_eq!(before, readback(&resolver));
}
