//! Behavioural tests for the document-manipulation vocabulary: what each verb writes, and what a
//! removal or a rename cascades into.

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

/// How far the shared address space reaches, pinned so that widening it is a deliberate edit and
/// not a drift: **setting a value** reaches a pipe, **wiring** does not. The advertised sentence
/// scopes its claim to this verb for exactly this reason.
#[test]
fn only_the_value_verb_reaches_a_pipe_address() {
    let registry = Registry::builtin();
    let resolver = resolver_with(&seed_with_pipe());

    set_instrument_input(SRC, "/cutoff", "in", json!(880.0), &registry, &resolver)
        .expect("setting a value reaches the pipe");

    for (verb, err) in [
        (
            "unwire",
            unwire_instrument_input(SRC, "/cutoff", "in", &registry, &resolver).unwrap_err(),
        ),
        (
            "wire",
            wire_instrument_input(SRC, "/cutoff", "in", "/osc", &registry, &resolver).unwrap_err(),
        ),
    ] {
        assert!(
            matches!(err, EditError::Target(_)),
            "{verb} addresses nodes only, and says so: {err:?}"
        );
    }

    // The constant verb addresses a node and names a plan-time slot on it; a pipe has no `config`
    // block for it to reach, so its absence from the shared space is the type, not an omission.
    let err = set_instrument_constant(SRC, "/cutoff", "voices", json!(4), &registry, &resolver)
        .expect_err("a pipe carries no constants");
    assert!(matches!(err, EditError::Target(_)), "got {err:?}");
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
    // The minimal required document and nothing else — see rules: agent-mcp
    assert_eq!(
        readback(&resolver),
        json!({ "format_version": 3, "instrument": "fresh", "nodes": [] }),
        "new_instrument writes exactly the minimal valid document"
    );

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
