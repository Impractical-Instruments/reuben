//! A `str` input end to end: an Operator declaring one, fed a document default through a `str`
//! interface pipe, a node literal, and live control messages; the load-time wire check refusing a
//! string into a number; the save round-trip; the authoring verbs; and what the render thread
//! allocates and frees while strings change under it.
//!
//! No built-in Operator declares a `str` input, so the fixture `str_len` is registered here: it
//! reports its input's character count on a held `f32` output.

mod rt_alloc;

use rt_alloc::{assert_counter_is_live, measure, Counting};

use std::sync::Arc;

use reuben_core::descriptor::Descriptor;
use reuben_core::message::{Arg, Message};
use reuben_core::operator::{Io, Operator};
use reuben_core::plan::Plan;
use reuben_core::{AudioConfig, Engine, Registry};
use reuben_document::edit::{
    add_instrument_interface_input, set_instrument_constant, set_instrument_input,
};
use reuben_document::{load_instrument, LoadError, MemoryResolver, NormalizedDoc};
use serde_json::json;

#[global_allocator]
static GLOBAL: Counting = Counting;

mod str_len {
    use super::*;

    /// The last length sent, so a held input re-sends nothing.
    pub struct StrLen(pub Option<f32>);
    reuben_core::operator_contract!(StrLen {
        type_name: "str_len",
        inputs: { text: str },
        outputs: { len: f32 { 0.0..=1_000_000.0, default 0.0 } },
        constants: { label: str },
    });

    impl Operator for StrLen {
        fn descriptor() -> Descriptor {
            Self::contract()
        }
        fn process(&mut self, io: &mut Io) {
            let n = io.read(IN_TEXT).chars().count() as f32;
            if self.0 != Some(n) {
                io.write(OUT_LEN).set(0, n);
                self.0 = Some(n);
            }
        }
        fn spawn(&self) -> Box<dyn Operator> {
            Box::new(StrLen(None))
        }
    }
}

fn registry() -> Registry {
    let mut r = Registry::builtin();
    r.register(
        || Box::new(str_len::StrLen(None)),
        <str_len::StrLen as Operator>::descriptor(),
    );
    r
}

#[test]
fn the_contract_declares_str_ports() {
    use reuben_core::descriptor::PortType;
    let d = <str_len::StrLen as Operator>::descriptor();
    assert_eq!(d.inputs[str_len::IN_TEXT.index()].ty, PortType::Str);
    assert_eq!(d.constants[str_len::C_LABEL].ty, PortType::Str);
}

/// `/pat` is a `str` pipe declaring a default, feeding `/from_pipe`; `/literal` carries a node
/// literal. Each reports to its own `osc_out` sink.
const DOC: &str = r#"{
  "format_version": 3,
  "instrument": "str_input",
  "interface": { "inputs": { "pat": { "type": "str", "default": "0 3 <7 5> ~" } } },
  "nodes": [
    { "type": "str_len", "address": "/from_pipe", "inputs": { "text": { "from": "/pat" } } },
    { "type": "str_len", "address": "/literal", "inputs": { "text": "hello" },
      "config": { "label": "lead" } },
    { "type": "str_len", "address": "/unset" },
    { "type": "osc_out", "address": "/pipe_len", "inputs": { "in": { "from": "/from_pipe.len" } } },
    { "type": "osc_out", "address": "/literal_len", "inputs": { "in": { "from": "/literal.len" } } },
    { "type": "osc_out", "address": "/unset_len", "inputs": { "in": { "from": "/unset.len" } } }
  ]
}"#;

fn engine(doc: &str) -> Engine {
    let graph = load_instrument(doc, &registry(), &MemoryResolver::new())
        .expect("loads")
        .graph;
    Engine::new(Plan::instantiate(graph, AudioConfig::new(48_000.0, 64)).expect("instantiates"))
}

/// One block; the `(sink address, f32)` pairs it sent, in order.
fn block(e: &mut Engine) -> Vec<(String, f32)> {
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    e.fill(&mut out);
    e.drain_outbound()
        .map(|m| (m.address, m.arg.as_f32().expect("a length")))
        .collect()
}

fn sent(pairs: &[(String, f32)], sink: &str) -> Vec<f32> {
    pairs
        .iter()
        .filter(|(a, _)| a == sink)
        .map(|(_, v)| *v)
        .collect()
}

#[test]
fn a_str_input_reads_its_document_default_then_each_live_send() {
    let mut e = engine(DOC);
    let first = block(&mut e);
    // "0 3 <7 5> ~" is 11 characters, "hello" 5, and an unset `str` input reads "".
    assert_eq!(sent(&first, "/pipe_len"), [11.0]);
    assert_eq!(sent(&first, "/literal_len"), [5.0]);
    assert_eq!(sent(&first, "/unset_len"), [0.0]);

    // Through the pipe's address, and straight onto a node input.
    e.queue_osc("/pat/in", &[Arg::Str("c4 e4 g4".into())]);
    e.queue_osc("/literal/text", &[Arg::Str("héllo wörld".into())]);
    let live = block(&mut e);
    assert_eq!(sent(&live, "/pipe_len"), [8.0]);
    assert_eq!(
        sent(&live, "/literal_len"),
        [11.0],
        "counts characters, not bytes"
    );

    // Held: nothing new, nothing re-sent, and the value stays.
    assert!(block(&mut e).is_empty());
    e.queue_osc("/pat/in", &[Arg::Str("~".into())]);
    assert_eq!(sent(&block(&mut e), "/pipe_len"), [1.0]);
}

#[test]
fn a_number_never_lands_on_a_str_input() {
    let mut e = engine(DOC);
    block(&mut e);
    // The flat-arg boundary refuses it outright.
    e.queue_osc("/pat/in", &[Arg::F32(3.0)]);
    e.queue_osc("/literal/text", &[Arg::I32(3)]);
    assert!(block(&mut e).is_empty());
    // A built Message skips that check; the port's own latch rule refuses it.
    e.queue(Message::new("/literal/text", Arg::F32(3.0), 0));
    e.queue(Message::new("/pat/in", Arg::F32(3.0), 0));
    assert!(block(&mut e).is_empty());
    // And a string still lands after the refusals.
    e.queue_osc("/literal/text", &[Arg::Str("abc".into())]);
    assert_eq!(sent(&block(&mut e), "/literal_len"), [3.0]);
}

fn load_err(doc: &str) -> Vec<LoadError> {
    match load_instrument(doc, &registry(), &MemoryResolver::new()) {
        Ok(_) => panic!("should be refused:\n{doc}"),
        Err(errs) => errs.into_iter().collect(),
    }
}

#[test]
fn the_wire_check_refuses_str_into_a_number_and_a_number_into_str() {
    let str_into_f32 = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str" } } },
      "nodes": [ { "type": "add_f32_value", "address": "/add", "inputs": { "a": { "from": "/pat" } } } ] }"#;
    let errs = load_err(str_into_f32);
    assert!(
        errs.iter().any(|e| e.to_string().contains("Str")),
        "{errs:?}"
    );

    let f32_into_str = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "x": { "type": "f32" } } },
      "nodes": [ { "type": "str_len", "address": "/n", "inputs": { "text": { "from": "/x" } } } ] }"#;
    let errs = load_err(f32_into_str);
    assert!(
        errs.iter().any(|e| e.to_string().contains("Str")),
        "{errs:?}"
    );

    // `osc_out`'s pass-through takes any type with an OSC form, a string included.
    let str_into_arg = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str" } } },
      "nodes": [ { "type": "osc_out", "address": "/o", "inputs": { "in": { "from": "/pat" } } } ] }"#;
    let mut e = engine(str_into_arg);
    e.queue_osc("/pat/in", &[Arg::Str("echo".into())]);
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    e.fill(&mut out);
    let echoed: Vec<Arg> = e.drain_outbound().map(|m| m.arg).collect();
    assert!(echoed.contains(&Arg::Str("echo".into())), "{echoed:?}");
}

#[test]
fn a_str_pipe_refuses_numeric_metadata() {
    for decl in [
        r#"{ "type": "str", "default": 3 }"#,
        r#"{ "type": "str", "min": 0 }"#,
        r#"{ "type": "str", "curve": "exp" }"#,
        r#"{ "type": "str", "channel": 0 }"#,
    ] {
        let doc = format!(
            r#"{{ "format_version": 3, "instrument": "t",
              "interface": {{ "inputs": {{ "pat": {decl} }} }}, "nodes": [] }}"#
        );
        let errs = load_err(&doc);
        assert!(
            matches!(errs.as_slice(), [LoadError::InterfacePipe { .. }]),
            "{decl}: {errs:?}"
        );
    }
}

#[test]
fn save_round_trips_every_str_value() {
    let registry = registry();
    let graph = load_instrument(DOC, &registry, &MemoryResolver::new())
        .expect("loads")
        .graph;
    let saved = NormalizedDoc::from_graph(&graph, "str_input", &registry).expect("flattens");
    let v = serde_json::to_value(saved.into_inner()).expect("serializes");
    assert_eq!(
        v["interface"]["inputs"]["pat"],
        json!({ "type": "str", "default": "0 3 <7 5> ~" })
    );
    let literal = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["address"] == "/literal")
        .expect("/literal");
    assert_eq!(literal["inputs"]["text"], json!("hello"));
    assert_eq!(literal["config"]["label"], json!("lead"));

    // And what was saved loads back to the same values.
    let again = serde_json::to_string(&v).unwrap();
    let mut e = engine(&again);
    let first = block(&mut e);
    assert_eq!(sent(&first, "/pipe_len"), [11.0]);
    assert_eq!(sent(&first, "/literal_len"), [5.0]);
}

const SRC: &str = "doc.json";

fn resolver_with(doc: &str) -> MemoryResolver {
    let mut r = MemoryResolver::new();
    r.insert_text(SRC, doc);
    r
}

fn readback(r: &MemoryResolver) -> serde_json::Value {
    use reuben_document::ResourceResolver;
    serde_json::from_str(&r.resolve_text(SRC).unwrap()).unwrap()
}

#[test]
fn the_authoring_verbs_write_and_refuse_str_values() {
    let registry = registry();
    let r = resolver_with(DOC);

    let done =
        set_instrument_input(SRC, "/unset", "text", json!("a b c"), &registry, &r).expect("runs");
    assert!(done.written, "{:?}", done.report);
    let done =
        set_instrument_input(SRC, "/pat", "in", json!("bd*2 sn"), &registry, &r).expect("runs");
    assert!(done.written, "{:?}", done.report);
    let done =
        set_instrument_constant(SRC, "/unset", "label", json!("pad"), &registry, &r).expect("runs");
    assert!(done.written, "{:?}", done.report);
    let done = add_instrument_interface_input(
        SRC,
        "words",
        "str",
        None,
        Some(json!("one two")),
        None,
        None,
        None,
        None,
        &registry,
        &r,
    )
    .expect("runs");
    assert!(done.written, "{:?}", done.report);

    let v = readback(&r);
    let unset = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["address"] == "/unset")
        .unwrap();
    assert_eq!(unset["inputs"]["text"], json!("a b c"));
    assert_eq!(unset["config"]["label"], json!("pad"));
    assert_eq!(v["interface"]["inputs"]["pat"]["default"], json!("bd*2 sn"));
    assert_eq!(
        v["interface"]["inputs"]["words"],
        json!({ "type": "str", "default": "one two" })
    );

    // A number is refused on a `str` input, a `str` constant and a `str` pipe, and nothing is
    // written.
    for refused in [
        set_instrument_input(SRC, "/unset", "text", json!(3), &registry, &r),
        set_instrument_input(SRC, "/pat", "in", json!(3), &registry, &r),
        set_instrument_constant(SRC, "/unset", "label", json!(3), &registry, &r),
    ] {
        let refused = refused.expect("runs");
        assert!(!refused.written, "{:?}", refused.report);
    }
    assert_eq!(readback(&r), v);
}

/// The render thread never allocates a string. It may free one: when a send replaces a latched
/// string and nothing else still holds it, the render thread drops the last reference. That is the
/// accepted class `Arg::Str` documents — one free per replaced string, none in a block where no
/// string changed, and none at all while the sender keeps its own reference.
#[test]
fn changing_strings_allocate_nothing_and_free_only_the_replaced_ones() {
    // No `osc_out` sinks: each outbound send builds a Message on the render thread, which is that
    // sink's own documented cost, not a string's.
    const QUIET: &str = r#"{
      "format_version": 3, "instrument": "quiet",
      "interface": { "inputs": { "pat": { "type": "str", "default": "seed" } } },
      "nodes": [
        { "type": "str_len", "address": "/from_pipe", "inputs": { "text": { "from": "/pat" } } },
        { "type": "str_len", "address": "/direct", "inputs": { "text": "x" } } ] }"#;
    let mut e = engine(QUIET);
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    // Warm up with sends too: the first message a node is routed grows its per-block scratch.
    // The sender holds on to these, like the ones below.
    let warm: Vec<Arc<str>> = (0..4).map(|i| format!("warm {i}").into()).collect();
    for s in &warm {
        e.queue_osc("/pat/in", &[Arg::Str(s.clone())]);
        e.queue_osc("/direct/text", &[Arg::Str(s.clone())]);
        e.fill(&mut out);
    }
    assert_counter_is_live();

    // Steady state, no sends: nothing at all.
    let idle = measure(|| {
        for _ in 0..16 {
            e.fill(&mut out);
        }
    });
    assert_eq!((idle.allocs, idle.frees), (0, 0), "idle blocks");

    // The sender keeps its own reference to every string it sends, so the render thread is
    // never the last owner of a replaced one: nothing is freed there.
    let kept: Vec<Arc<str>> = (0..16).map(|i| format!("kept {i}").into()).collect();
    let mut keeping = rt_alloc::Counts {
        allocs: 0,
        frees: 0,
    };
    for s in &kept {
        let c = measure(|| {
            e.queue_osc("/pat/in", &[Arg::Str(s.clone())]);
            e.queue_osc("/direct/text", &[Arg::Str(s.clone())]);
            e.fill(&mut out);
        });
        keeping.allocs += c.allocs;
        keeping.frees += c.frees;
    }
    assert_eq!((keeping.allocs, keeping.frees), (0, 0), "sender retains");

    // The sender lets go of each string as soon as it is queued, so the graph holds the last
    // reference to whatever is latched. Each block then frees exactly the strings its sends
    // replaced: the first block one (both ports held the same last `kept` string), every later
    // block two (the previous block's pair).
    drop((warm, kept));
    let mut per_block = Vec::new();
    for i in 0..16 {
        let a: Arc<str> = format!("pipe {i}").into();
        let b: Arc<str> = format!("direct {i}").into();
        e.queue_osc("/pat/in", &[Arg::Str(a)]);
        e.queue_osc("/direct/text", &[Arg::Str(b)]);
        per_block.push(measure(|| e.fill(&mut out)));
    }
    assert!(
        per_block.iter().all(|c| c.allocs == 0),
        "the render thread never allocates a string: {per_block:?}"
    );
    let frees: Vec<usize> = per_block.iter().map(|c| c.frees).collect();
    let mut owed = vec![2; 16];
    owed[0] = 1;
    assert_eq!(frees, owed, "one free per replaced string, and no other");

    // A block with no send frees nothing, even right after a replacement.
    let after = measure(|| e.fill(&mut out));
    assert_eq!((after.allocs, after.frees), (0, 0), "after the sends");

    // Two sends to one port in one block: the first is overwritten within the block, but the
    // routed copy still holds it until the next block clears its scratch. Its block frees the
    // previously latched string; the next frees the overwritten one; then nothing.
    e.queue_osc("/direct/text", &[Arg::Str("first".into())]);
    e.queue_osc("/direct/text", &[Arg::Str("second".into())]);
    let frees: Vec<usize> = (0..3).map(|_| measure(|| e.fill(&mut out)).frees).collect();
    assert_eq!(
        frees,
        [1, 1, 0],
        "an overwrite within a block frees one block late"
    );
}
