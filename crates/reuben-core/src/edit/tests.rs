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
