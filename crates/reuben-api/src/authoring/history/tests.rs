use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use std::cell::RefCell;

use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::*;
use crate::authoring::*;

/// Sources are exact keys, except that a leading `./` is spelling: enough of a `canonical` to show
/// which identity the history keys on.
#[derive(Default)]
struct MemoryStore {
    texts: RefCell<BTreeMap<String, String>>,
}

impl MemoryStore {
    fn with(source: &str, text: &str) -> Self {
        let store = Self::default();
        store.put(source, text);
        store
    }

    fn put(&self, source: &str, text: &str) {
        self.texts
            .borrow_mut()
            .insert(key(source), text.to_string());
    }

    fn read(&self, source: &str) -> String {
        self.texts.borrow()[&key(source)].clone()
    }
}

fn key(source: &str) -> String {
    source.strip_prefix("./").unwrap_or(source).to_string()
}

impl Resources for MemoryStore {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        self.texts
            .borrow()
            .get(&key(source))
            .cloned()
            .ok_or_else(|| ResolveError::NotFound(source.to_string()))
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.put(source, text);
        Ok(())
    }

    fn canonical(&self, source: &str, _referrer: Option<&str>) -> String {
        key(source)
    }
}

#[test]
fn the_default_store_keeps_no_surface_docs() {
    use crate::surface::SurfaceKind;
    let store = MemoryStore::with(SRC, &seed());
    assert_eq!(store.surface_source(SRC, SurfaceKind::Screen), None);
    assert_eq!(store.surface_source(SRC, SurfaceKind::Midi), None);
}

const SRC: &str = "inst.json";

fn seed() -> String {
    json!({
        "format_version": 3,
        "instrument": "test",
        "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": 220.0 } },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/osc" }, "b": 0.5 } }
        ],
        "interface": { "outputs": { "main": { "from": "/amp" } } }
    })
    .to_string()
}

fn history() -> DocumentHistory<MemoryStore> {
    DocumentHistory::new(MemoryStore::with(SRC, &seed()))
}

/// Write `freq` to `/osc` at `source` and return the hash the edit reports.
fn set_freq(source: &str, freq: f64, resources: &dyn Resources) -> String {
    let answer = set_instrument_input(
        &SetInstrumentInput {
            source: source.to_string(),
            address: "/osc".to_string(),
            input: "freq".to_string(),
            value: json!(freq),
            expect: None,
        },
        resources,
    )
    .expect("an edit to a real node");
    assert!(answer.output.written, "{:?}", answer.output);
    answer.output.hash
}

fn restore(source: &str, to: &str, expect: &str) -> RestoreInstrument {
    RestoreInstrument {
        source: source.to_string(),
        to: to.to_string(),
        expect: expect.to_string(),
    }
}

fn freq_at(store: &MemoryStore, source: &str) -> Value {
    let doc: Value = serde_json::from_str(&store.read(source)).expect("a document");
    doc["nodes"][0]["inputs"]["freq"].clone()
}

#[test]
fn undo_then_redo_each_land_on_the_hash_asked_for() {
    let history = history();
    let first = set_freq(SRC, 330.0, &history);
    let second = set_freq(SRC, 440.0, &history);

    let undone = restore_instrument(&restore(SRC, &first, &second), &history)
        .expect("the first edit's document is held")
        .output;
    assert!(undone.written, "{undone:?}");
    assert_eq!(undone.hash, first);
    assert_eq!(freq_at(history.inner(), SRC), json!(330.0));

    // The redo's target is the state the undo overwrote.
    let redone = restore_instrument(&restore(SRC, &second, &first), &history)
        .expect("the second edit's document is held")
        .output;
    assert!(redone.written, "{redone:?}");
    assert_eq!(redone.hash, second);
    assert_eq!(freq_at(history.inner(), SRC), json!(440.0));
}

#[test]
fn the_state_before_the_first_write_through_the_store_is_held() {
    let history = history();
    let original = hash_instrument(&seed(), &history).expect("the seed hashes");
    let edited = set_freq(SRC, 330.0, &history);

    let back = restore_instrument(&restore(SRC, &original, &edited), &history)
        .expect("the pre-write text was recorded")
        .output;
    assert!(back.written, "{back:?}");
    assert_eq!(back.hash, original);
    assert_eq!(freq_at(history.inner(), SRC), json!(220.0));
}

#[test]
fn an_expect_mismatch_writes_nothing_and_reports_what_is_on_disk() {
    let history = history();
    let first = set_freq(SRC, 330.0, &history);
    let second = set_freq(SRC, 440.0, &history);
    let on_disk = history.inner().read(SRC);

    let missed = restore_instrument(&restore(SRC, &first, "deadbeefdeadbeef"), &history)
        .expect("a guard miss is an answer, not a refusal")
        .output;
    assert!(!missed.written);
    assert!(!missed.report.ok);
    assert_eq!(missed.hash, second);
    assert_eq!(history.inner().read(SRC), on_disk);
}

#[test]
fn a_hash_never_seen_is_refused() {
    let history = history();
    let current = set_freq(SRC, 330.0, &history);
    let on_disk = history.inner().read(SRC);

    let refusal = restore_instrument(&restore(SRC, "deadbeefdeadbeef", &current), &history)
        .expect_err("nothing is held under that hash");
    assert!(refusal.message.contains("deadbeefdeadbeef"), "{refusal}");
    assert!(refusal.message.contains("held"), "{refusal}");
    assert_eq!(history.inner().read(SRC), on_disk);
}

#[test]
fn an_evicted_hash_is_refused_and_the_newest_survive() {
    let history = DocumentHistory::with_capacity(MemoryStore::with(SRC, &seed()), 2);
    let original = hash_instrument(&seed(), &history).expect("the seed hashes");
    let first = set_freq(SRC, 330.0, &history);
    let second = set_freq(SRC, 440.0, &history);

    let refusal = restore_instrument(&restore(SRC, &original, &second), &history)
        .expect_err("two newer documents pushed the original out");
    assert!(refusal.message.contains("evicted"), "{refusal}");

    let undone = restore_instrument(&restore(SRC, &first, &second), &history)
        .expect("the two most recent are held")
        .output;
    assert_eq!(undone.hash, first);
}

#[test]
fn the_bound_is_shared_across_sources_oldest_first() {
    let store = MemoryStore::with("a.json", &seed());
    store.put("b.json", &seed());
    let history = DocumentHistory::with_capacity(store, 3);
    let a_original = hash_instrument(&seed(), &history).expect("the seed hashes");
    let a_edited = set_freq("a.json", 330.0, &history);
    // Two more documents, at another source, into a store already holding two.
    let b_edited = set_freq("b.json", 550.0, &history);

    restore_instrument(&restore("a.json", &a_original, &a_edited), &history)
        .expect_err("a.json's oldest document went first");
    let b_back = restore_instrument(&restore("b.json", &a_original, &b_edited), &history)
        .expect("b.json's own pre-write text is held")
        .output;
    assert!(b_back.written, "{b_back:?}");
}

#[test]
fn a_hash_seen_only_at_another_source_is_refused() {
    let store = MemoryStore::with("a.json", &seed());
    store.put("b.json", &seed());
    let history = DocumentHistory::new(store);
    let a_edited = set_freq("a.json", 330.0, &history);
    let b_edited = set_freq("b.json", 550.0, &history);

    let refusal = restore_instrument(&restore("b.json", &a_edited, &b_edited), &history)
        .expect_err("b.json was never in a.json's state");
    assert!(refusal.message.contains("b.json"), "{refusal}");
    assert_eq!(freq_at(history.inner(), "b.json"), json!(550.0));
}

#[test]
fn two_spellings_of_one_source_share_its_documents() {
    let history = history();
    let first = set_freq(SRC, 330.0, &history);
    let second = set_freq("./inst.json", 440.0, &history);

    let undone = restore_instrument(&restore("./inst.json", &first, &second), &history)
        .expect("held under the canonical source, whichever spelling wrote it")
        .output;
    assert_eq!(undone.hash, first);
}

/// A child whose only interface output is `name`.
fn child(output: &str) -> String {
    json!({
        "format_version": 3,
        "instrument": "child",
        "nodes": [ { "type": "oscillator", "address": "/osc" } ],
        "interface": { "outputs": { output: { "from": "/osc" } } }
    })
    .to_string()
}

#[test]
fn a_prior_document_a_referenced_file_has_since_broken_writes_nothing_and_reports() {
    let parent = json!({
        "format_version": 3,
        "instrument": "parent",
        "resources": { "voice": "child.json" },
        "nodes": [
            { "type": "subpatch", "address": "/sub", "patch": "voice" },
            { "type": "mul_f32_signal", "address": "/amp",
              "inputs": { "a": { "from": "/sub.out" }, "b": 0.5 } }
        ],
        "interface": { "outputs": { "main": { "from": "/amp" } } }
    })
    .to_string();
    let store = MemoryStore::with(SRC, &parent);
    store.put("child.json", &child("out"));
    let history = DocumentHistory::new(store);
    let wired = hash_instrument(&parent, &history).expect("the parent hashes");

    let unwired = unwire_instrument_input(
        &UnwireInstrumentInput {
            source: SRC.to_string(),
            address: "/amp".to_string(),
            input: "a".to_string(),
            expect: None,
        },
        &history,
    )
    .expect("the wire exists")
    .output;
    assert!(unwired.written, "{unwired:?}");
    // The child stops offering the port the wired parent read.
    history.inner().put("child.json", &child("renamed"));
    let on_disk = history.inner().read(SRC);

    let result = restore_instrument(&restore(SRC, &wired, &unwired.hash), &history)
        .expect("the verb ran: an invalid result is an answer")
        .output;
    assert!(!result.written);
    assert!(!result.report.ok);
    assert!(!result.report.errors.is_empty());
    assert_eq!(result.hash, unwired.hash);
    assert_eq!(history.inner().read(SRC), on_disk);
}

#[test]
fn a_source_that_does_not_parse_cannot_be_restored_over() {
    let history = history();
    let original = hash_instrument(&seed(), &history).expect("the seed hashes");
    let edited = set_freq(SRC, 330.0, &history);
    history.inner().put(SRC, "{ not a document");

    let refusal = restore_instrument(&restore(SRC, &original, &edited), &history)
        .expect_err("an unparseable file has no hash for `expect` to match");
    assert!(
        refusal.message.contains("not a loadable document"),
        "{refusal}"
    );
    assert_eq!(history.inner().read(SRC), "{ not a document");
}

/// One step of the script below: a verb, and whatever it answered rendered for comparison.
fn call<A: DeserializeOwned, T: core::fmt::Debug>(
    args: Value,
    verb: impl FnOnce(&A) -> Result<Answer<T>, Refusal>,
) -> String {
    let args = serde_json::from_value(args).expect("the script's arguments parse");
    format!("{:?}", verb(&args))
}

/// Every verb that takes a `Resources`, run in an order where most of them land. Returns each
/// answer in order.
fn run_every_verb(r: &dyn Resources) -> Vec<String> {
    let s = SRC;
    let mut out = Vec::new();
    out.push(call(
        json!({ "source": "new.json", "name": "fresh" }),
        |a| new_instrument(a, r),
    ));
    out.push(call(json!({ "source": s, "name": "renamed" }), |a| {
        set_instrument_name(a, r)
    }));
    out.push(call(json!({ "source": s, "description": "a test" }), |a| {
        set_instrument_description(a, r)
    }));
    out.push(call(
        json!({ "source": s, "address": "/lfo", "type": "oscillator" }),
        |a| add_instrument_node(a, r),
    ));
    out.push(call(
        json!({ "source": s, "from": "/lfo", "to": "/mod" }),
        |a| rename_instrument_node(a, r),
    ));
    out.push(call(
        json!({ "source": s, "address": "/mod", "description": "wobble" }),
        |a| set_instrument_node_description(a, r),
    ));
    out.push(call(
        json!({ "source": s, "address": "/osc", "input": "freq", "value": 330.0 }),
        |a| set_instrument_input(a, r),
    ));
    out.push(call(json!({ "source": s, "word": "brighter" }), |a| {
        set_instrument_inputs_by_intent(a, r)
    }));
    out.push(call(
        json!({ "source": s, "name": "cutoff", "type": "f32", "value": 440.0 }),
        |a| add_instrument_interface_input(a, r),
    ));
    out.push(call(
        json!({ "source": s, "address": "/osc", "input": "freq", "from": "/cutoff" }),
        |a| wire_instrument_input(a, r),
    ));
    out.push(call(
        json!({ "source": s, "name": "cutoff", "unit": "Hz" }),
        |a| set_instrument_interface_input_meta(a, r),
    ));
    out.push(call(
        json!({ "source": s, "name": "cutoff", "direction": "input", "fields": ["unit"] }),
        |a| clear_instrument_interface_meta(a, r),
    ));
    out.push(call(
        json!({ "source": s, "address": "/osc", "input": "freq" }),
        |a| unwire_instrument_input(a, r),
    ));
    out.push(call(json!({ "source": s, "name": "cutoff" }), |a| {
        remove_instrument_interface_input(a, r)
    }));
    out.push(call(
        json!({ "source": s, "name": "tap", "from": "/osc" }),
        |a| add_instrument_interface_output(a, r),
    ));
    out.push(call(
        json!({ "source": s, "name": "tap", "unit": "V" }),
        |a| set_instrument_interface_output_meta(a, r),
    ));
    out.push(call(
        json!({ "source": s, "name": "tap", "from": "/amp" }),
        |a| wire_instrument_interface_output(a, r),
    ));
    out.push(call(json!({ "source": s, "name": "tap" }), |a| {
        remove_instrument_interface_output(a, r)
    }));
    out.push(call(
        json!({ "source": s, "address": "/voicer", "type": "voicer" }),
        |a| add_instrument_node(a, r),
    ));
    out.push(call(
        json!({ "source": s, "address": "/voicer", "name": "voices", "value": 4 }),
        |a| set_instrument_constant(a, r),
    ));
    out.push(call(
        json!({ "source": s, "address": "/voicer", "name": "voices" }),
        |a| remove_instrument_constant(a, r),
    ));
    out.push(call(
        json!({ "source": s, "id": "kick", "resource_source": "kick.wav" }),
        |a| add_instrument_resource(a, r),
    ));
    out.push(call(
        json!({ "source": s, "id": "kick", "resource_source": "kick-2.wav" }),
        |a| set_instrument_resource(a, r),
    ));
    out.push(call(json!({ "source": s, "id": "kick" }), |a| {
        remove_instrument_resource(a, r)
    }));
    out.push(call(
        json!({ "source": s, "id": "kick", "resource_source": "kick.wav" }),
        |a| add_instrument_resource(a, r),
    ));
    out.push(call(
        json!({ "source": s, "address": "/hit", "type": "sample", "sample": "kick" }),
        |a| add_instrument_node(a, r),
    ));
    out.push(call(
        json!({ "source": s, "id": "snare", "resource_source": "snare.wav" }),
        |a| add_instrument_resource(a, r),
    ));
    out.push(call(
        json!({ "source": s, "address": "/hit", "slot": "sample", "id": "snare" }),
        |a| set_instrument_node_resource(a, r),
    ));
    out.push(call(json!({ "source": s, "address": "/hit" }), |a| {
        remove_instrument_node(a, r)
    }));
    out.push(call(
        json!({ "source": s, "positions": [ { "target": "/osc", "x": 1.0, "y": 2.0 } ] }),
        |a| set_instrument_layout(a, r),
    ));
    out.push(call(json!({ "source": s, "address": "/mod" }), |a| {
        remove_instrument_node(a, r)
    }));
    // The three ways a verb does not write: a guard miss, an invalid result, a refusal.
    out.push(call(
        json!({ "source": s, "name": "late", "expect": "deadbeefdeadbeef" }),
        |a| set_instrument_name(a, r),
    ));
    out.push(call(
        json!({ "source": s, "address": "/amp", "input": "a", "from": "/ghost" }),
        |a| wire_instrument_input(a, r),
    ));
    out.push(call(json!({ "source": s, "address": "/ghost" }), |a| {
        remove_instrument_node(a, r)
    }));
    out.push(call(json!({ "source": s }), |a| describe_instrument(a, r)));
    out.push(call(json!({ "source": s }), |a| describe_boundary(a, r)));
    out.push(call(json!({ "source": s }), |a| validate_instrument(a, r)));
    out.push(call(
        json!({ "source": s, "from": { "kind": "node_output", "node": "/osc", "port": "audio" } }),
        |a| wire_compatibility(a, r),
    ));
    out.push(format!("{:?}", library_index_line(s, r)));
    out.push(format!("{:?}", hash_instrument(&seed(), r)));
    out
}

#[test]
fn every_verb_answers_the_same_through_the_history_as_through_the_bare_store() {
    let bare = MemoryStore::with(SRC, &seed());
    let history = history();

    let through_bare = run_every_verb(&bare);
    let through_history = run_every_verb(&history);

    for (i, (bare, wrapped)) in through_bare.iter().zip(&through_history).enumerate() {
        assert_eq!(bare, wrapped, "step {i}");
    }
    assert_eq!(*bare.texts.borrow(), *history.inner().texts.borrow());
    // The comparison means nothing if the script mostly bounced off.
    let written = through_bare
        .iter()
        .filter(|a| a.contains("written: true"))
        .count();
    assert!(
        written >= 20,
        "only {written} steps wrote:\n{through_bare:#?}"
    );
}

#[test]
fn no_roster_serves_restore() {
    assert!(!crate::tools::CONTRACTS
        .iter()
        .any(|c| c.name == "restore_instrument" && c.kind != crate::tools::ContractKind::Host));
}

/// A v1 host whose input pipe takes its type from the nested child, so what its text normalizes to
/// follows the child.
const V1_HOST: &str = r#"{
    "instrument": "host",
    "resources": { "c": "child.json" },
    "interface": { "inputs": { "freq": "/nest.freq" } },
    "nodes": [ { "type": "subpatch", "address": "/nest", "patch": "c" } ]
}"#;

fn typed_child(pipe_type: &str) -> String {
    json!({
        "format_version": 3,
        "instrument": "child",
        "interface": {
            "inputs": { "freq": { "type": pipe_type, "min": 20, "max": 20000, "default": 440 } },
            "outputs": { "audio": { "from": "/osc.audio" } }
        },
        "nodes": [ { "type": "oscillator", "address": "/osc",
                     "inputs": { "freq": { "from": "/freq" } } } ]
    })
    .to_string()
}

#[test]
fn a_restore_lands_on_the_hash_asked_for_after_a_file_the_prior_text_was_migrated_against_changes()
{
    let store = MemoryStore::with("host.json", V1_HOST);
    store.put("child.json", &typed_child("f32"));
    let history = DocumentHistory::new(store);
    let before = hash_instrument(V1_HOST, &history).expect("a v1 document");
    let after = set_instrument_name(
        &SetInstrumentName {
            source: "host.json".to_string(),
            name: "renamed".to_string(),
            expect: None,
        },
        &history,
    )
    .expect("an edit")
    .output
    .hash;
    history
        .inner()
        .put("child.json", &typed_child("f32_buffer"));

    let restored = restore_instrument(&restore("host.json", &before, &after), &history)
        .expect("held")
        .output;

    assert!(restored.written, "{restored:?}");
    assert_eq!(restored.hash, before);
}

/// A store whose document is replaced from outside just before its `at`-th read.
struct Raced {
    store: MemoryStore,
    reads: core::cell::Cell<usize>,
    at: usize,
    external: String,
}

impl Resources for Raced {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.store.read_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        self.reads.set(self.reads.get() + 1);
        if self.reads.get() == self.at {
            self.store.put(SRC, &self.external);
        }
        self.store.read_text(source)
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.store.write_text(source, text)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.store.canonical(source, referrer)
    }
}

#[test]
fn an_external_write_between_the_guard_and_the_restores_own_read_is_not_overwritten() {
    let external = seed().replace("220.0", "999.0");
    assert_ne!(external, seed());
    let history = DocumentHistory::new(Raced {
        store: MemoryStore::with(SRC, &seed()),
        reads: core::cell::Cell::new(0),
        at: usize::MAX,
        external,
    });
    let before = hash_instrument(&seed(), &history).expect("a document");
    let after = set_freq(SRC, 330.0, &history);
    // The guard takes the next read; the restore's own read is the one after it.
    let raced = Raced {
        at: history.inner().reads.get() + 2,
        ..history.into_inner()
    };
    let history = {
        let fresh = DocumentHistory::new(raced);
        // Hold `before` again: the new wrapper starts empty.
        fresh.record(SRC, before.clone(), seed());
        fresh
    };

    let answer = restore_instrument(&restore(SRC, &before, &after), &history)
        .expect("the guard's answer")
        .output;

    assert!(!answer.written, "{answer:?}");
    assert_ne!(answer.hash, after);
    assert_eq!(freq_at(&history.inner().store, SRC), json!(999.0));
}

#[test]
fn a_state_returned_to_is_not_the_next_evicted() {
    let history = DocumentHistory::with_capacity(MemoryStore::with(SRC, &seed()), 3);
    let first = hash_instrument(&seed(), &history).expect("a document");
    set_freq(SRC, 330.0, &history);
    let third = set_freq(SRC, 440.0, &history);
    let back = restore_instrument(&restore(SRC, &first, &third), &history)
        .expect("held")
        .output;
    assert!(back.written, "{back:?}");
    // The oldest recording at the bound is now the second state, not the first.
    let fourth = set_freq(SRC, 550.0, &history);

    let again = restore_instrument(&restore(SRC, &first, &fourth), &history)
        .expect("the first state was recorded again by the restore")
        .output;

    assert!(again.written, "{again:?}");
}

#[test]
fn a_hash_not_held_is_refused_whatever_the_expect() {
    let history = history();
    set_freq(SRC, 330.0, &history);

    restore_instrument(&restore(SRC, "0000000000000000", "stale"), &history)
        .expect_err("nothing is held under that hash");
}

#[test]
fn holds_answers_for_a_source_and_hash_and_forgets_what_was_evicted() {
    let history = DocumentHistory::with_capacity(MemoryStore::with(SRC, &seed()), 2);
    let original = hash_instrument(&seed(), &history).expect("the seed hashes");
    let first = set_freq(SRC, 330.0, &history);
    assert!(history.holds(SRC, &original));
    assert!(
        history.holds(&format!("./{SRC}"), &first),
        "by canonical source"
    );
    assert!(
        !history.holds("other.json", &first),
        "held for this source only"
    );

    set_freq(SRC, 440.0, &history);
    assert!(!history.holds(SRC, &original), "evicted");
    assert!(history.holds(SRC, &first));
}

/// A store whose create is its own, so the history is seen to hand a create on rather than
/// falling back to a read and a write.
struct CreateOnly(MemoryStore);

impl Resources for CreateOnly {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.0.read_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        self.0.read_text(source)
    }

    fn write_text(&self, source: &str, _text: &str) -> Result<(), ResolveError> {
        Err(ResolveError::Write(format!("{source}: only created here")))
    }

    fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
        if self.0.read_text(source).is_ok() {
            return Ok(Created::Taken);
        }
        self.0.put(source, text);
        Ok(Created::Written)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.0.canonical(source, referrer)
    }
}

#[test]
fn a_create_goes_to_the_wrapped_store_and_what_it_created_is_held() {
    let history = DocumentHistory::new(CreateOnly(MemoryStore::default()));
    assert!(matches!(
        history.create_text(SRC, &seed()),
        Ok(Created::Written)
    ));
    let hash = hash_instrument(&seed(), &history).expect("the seed hashes");
    assert!(history.holds(SRC, &hash));

    assert!(matches!(history.create_text(SRC, "{}"), Ok(Created::Taken)));
    assert_eq!(
        history.inner().0.read(SRC),
        seed(),
        "a taken source is left as it was"
    );
}

const MAP: &str = "inst.midi.json";

fn surface_text(bind: &str) -> String {
    use crate::surface::{MidiKind, MidiSource, NumberPattern, SurfaceControl, SurfaceDoc};
    let doc = SurfaceDoc {
        instrument: "inst".to_string(),
        controls: vec![SurfaceControl {
            bind: bind.to_string(),
            midi: Some(MidiSource {
                kind: MidiKind::Cc,
                channel: NumberPattern::Number(1),
                cc: Some(NumberPattern::Number(1)),
            }),
            ..SurfaceControl::default()
        }],
        ..SurfaceDoc::default()
    };
    doc.to_file_text()
}

fn surface_hash_of(text: &str) -> String {
    use crate::surface::{surface_hash, SurfaceDoc};
    surface_hash(&SurfaceDoc::parse(text).expect("a surface doc"))
}

#[test]
fn a_surface_write_is_held_and_restorable() {
    let history = history();
    let (a, b) = (surface_text("/freq/in"), surface_text("/gain/in"));
    let (hash_a, hash_b) = (surface_hash_of(&a), surface_hash_of(&b));
    history.write_text(MAP, &a).expect("write A");
    history.write_text(MAP, &b).expect("write B");

    let back = restore_instrument(&restore(MAP, &hash_a, &hash_b), &history)
        .expect("A is held")
        .output;
    assert!(back.written, "{back:?}");
    assert_eq!(back.hash, hash_a);
    assert_eq!(history.inner().read(MAP), a);
}

#[test]
fn a_surface_restore_with_a_stale_expect_is_a_conflict() {
    let history = history();
    let (a, b) = (surface_text("/freq/in"), surface_text("/gain/in"));
    let (hash_a, hash_b) = (surface_hash_of(&a), surface_hash_of(&b));
    history.write_text(MAP, &a).expect("write A");
    history.write_text(MAP, &b).expect("write B");

    let missed = restore_instrument(&restore(MAP, &hash_a, &hash_a), &history)
        .expect("a guard miss is an answer, not a refusal")
        .output;
    assert!(!missed.written);
    assert_eq!(missed.hash, hash_b);
    assert!(missed.report.errors[0].message.starts_with("expect guard:"));
    assert_eq!(history.inner().read(MAP), b);
}

#[test]
fn a_surface_restore_is_not_checked_against_any_instrument() {
    let history = history();
    let (a, b) = (surface_text("/nothing/has/this"), surface_text("/gain/in"));
    let (hash_a, hash_b) = (surface_hash_of(&a), surface_hash_of(&b));
    history.write_text(MAP, &a).expect("write A");
    history.write_text(MAP, &b).expect("write B");

    let back = restore_instrument(&restore(MAP, &hash_a, &hash_b), &history)
        .expect("A is held")
        .output;
    assert!(back.written, "{back:?}");
    assert_eq!(history.inner().read(MAP), a);
}
