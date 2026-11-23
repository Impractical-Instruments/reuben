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

use reuben_core::coordinator::{retire_queue, RetireDrain};
use reuben_core::descriptor::Descriptor;
use reuben_core::message::{Arg, Message};
use reuben_core::operator::{Io, Operator};
use reuben_core::plan::Plan;
use reuben_core::{AudioConfig, Engine, Registry};
use reuben_document::coordinator::RETIRE_QUEUE_CAPACITY;
use reuben_document::edit::{add_instrument_interface_input, set_instrument_input};
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
}

/// `/pat` is a `str` pipe declaring a default, feeding `/from_pipe`; `/literal` carries a node
/// literal. Each reports to its own `osc_out` sink.
const DOC: &str = r#"{
  "format_version": 3,
  "instrument": "str_input",
  "interface": { "inputs": { "pat": { "type": "str", "default": "0 3 <7 5> ~" } } },
  "nodes": [
    { "type": "str_len", "address": "/from_pipe", "inputs": { "text": { "from": "/pat" } } },
    { "type": "str_len", "address": "/literal", "inputs": { "text": "hello" } },
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

/// [`engine`] retiring into a queue, as a Coordinator-built Engine does, and the queue's drain.
fn retiring(mut e: Engine) -> (Engine, RetireDrain) {
    let (queue, drain) = retire_queue(RETIRE_QUEUE_CAPACITY);
    e.set_retire_queue(queue);
    (e, drain)
}

/// Drain `d` under the counting allocator: the frees the render thread handed off it.
fn drained_frees(d: &RetireDrain) -> usize {
    measure(|| {
        d.drain();
    })
    .frees
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

    // Across a subpatch boundary too: the face carries the child pipe's `str` type.
    const CHILD: &str = r#"{ "format_version": 3, "instrument": "c",
      "interface": { "inputs": { "pat": { "type": "str" } } },
      "nodes": [ { "type": "str_len", "address": "/n", "inputs": { "text": { "from": "/pat" } } } ] }"#;
    let mut r = MemoryResolver::new();
    r.insert_text("c.json", CHILD);
    let f32_into_child = r#"{ "format_version": 3, "instrument": "p", "resources": { "c": "c.json" },
      "interface": { "inputs": { "x": { "type": "f32" } } },
      "nodes": [ { "type": "subpatch", "address": "/sub", "patch": "c",
                   "inputs": { "pat": { "from": "/x" } } } ] }"#;
    let errs: Vec<LoadError> = match load_instrument(f32_into_child, &registry(), &r) {
        Ok(_) => panic!("f32 into a child's str boundary should be refused"),
        Err(errs) => errs.into_iter().collect(),
    };
    assert!(
        errs.iter().any(|e| e.to_string().contains("Str")),
        "{errs:?}"
    );
    let str_into_child = f32_into_child.replace(r#""type": "f32""#, r#""type": "str""#);
    load_instrument(&str_into_child, &registry(), &r).expect("str into a child's str boundary");

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
    assert_eq!(v["interface"]["inputs"]["pat"]["default"], json!("bd*2 sn"));
    assert_eq!(
        v["interface"]["inputs"]["words"],
        json!({ "type": "str", "default": "one two" })
    );

    // A number is refused on a `str` input and a `str` pipe, and nothing is written.
    for refused in [
        set_instrument_input(SRC, "/unset", "text", json!(3), &registry, &r),
        set_instrument_input(SRC, "/pat", "in", json!(3), &registry, &r),
    ] {
        let refused = refused.expect("runs");
        assert!(!refused.written, "{:?}", refused.report);
    }
    assert_eq!(readback(&r), v);
}

/// The render thread neither allocates nor frees a string. Each value it lets go of while strings
/// change under it — a latch's seed, an authored value, a string a send replaced, its own copies in
/// the control queue and routing scratch — goes to the retire queue, and the strings that were last
/// held there are freed where the queue is drained.
#[test]
fn changing_strings_allocate_and_free_nothing_on_the_render_thread() {
    // No `osc_out` sinks: each outbound send builds a Message on the render thread, which is that
    // sink's own documented cost, not a string's.
    const QUIET: &str = r#"{
      "format_version": 3, "instrument": "quiet",
      "interface": { "inputs": { "pat": { "type": "str", "default": "seed" } } },
      "nodes": [
        { "type": "str_len", "address": "/from_pipe", "inputs": { "text": { "from": "/pat" } } },
        { "type": "str_len", "address": "/direct", "inputs": { "text": "x" } },
        { "type": "str_len", "address": "/unset" } ] }"#;
    let (mut e, retired) = retiring(engine(QUIET));
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    for _ in 0..4 {
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

    // The first send to each input, one input per block, with the sender keeping every string it
    // sends. No allocation: each node's routed-held scratch is presized for one change per input.
    let warm: Arc<str> = "first send".into();
    let mut first_send = |addr: &str| {
        let render = measure(|| {
            e.queue_osc(addr, &[Arg::Str(warm.clone())]);
            e.fill(&mut out);
        });
        (render, drained_frees(&retired))
    };
    // An unset input's seed `""` and a node literal are held by nothing but their latch: the
    // overwrite retires each, and the drain frees it.
    let (unset, unset_drained) = first_send("/unset/text");
    assert_eq!(
        (unset.allocs, unset.frees, unset_drained),
        (0, 0, 1),
        "unset seed"
    );
    let (literal, literal_drained) = first_send("/direct/text");
    assert_eq!(
        (literal.allocs, literal.frees, literal_drained),
        (0, 0, 1),
        "node literal"
    );
    // A pipe's default handed to the input it dissolved into: whether anything else still holds
    // it depends on what the Plan keeps for the pipe's address, so only the bound is pinned.
    let (piped, piped_drained) = first_send("/pat/in");
    assert_eq!(
        (piped.allocs, piped.frees),
        (0, 0),
        "pipe default: no allocation and no free"
    );
    assert!(
        piped_drained <= 1,
        "pipe default: at most one free, got {piped_drained}"
    );

    // Past the first send, a sender that keeps its own reference to every string it sends leaves
    // the queue never the last owner of a replaced one: nothing is freed anywhere.
    let kept: Vec<Arc<str>> = (0..16).map(|i| format!("kept {i}").into()).collect();
    let mut keeping = (0, 0, 0);
    for s in &kept {
        let c = measure(|| {
            e.queue_osc("/pat/in", &[Arg::Str(s.clone())]);
            e.queue_osc("/direct/text", &[Arg::Str(s.clone())]);
            e.fill(&mut out);
        });
        keeping.0 += c.allocs;
        keeping.1 += c.frees;
        keeping.2 += drained_frees(&retired);
    }
    assert_eq!(keeping, (0, 0, 0), "sender retains");

    // The sender lets go of each string as soon as it is queued, so the graph holds the last
    // reference to whatever is latched. Repeated sends to the same two inputs: the render thread
    // frees nothing, and each drain frees exactly the strings that block's sends replaced — the
    // first block one (both ports held the same last `kept` string), every later block two (the
    // previous block's pair).
    drop((warm, kept));
    let mut render = Vec::new();
    let mut drained = Vec::new();
    for i in 0..16 {
        let a: Arc<str> = format!("pipe {i}").into();
        let b: Arc<str> = format!("direct {i}").into();
        e.queue_osc("/pat/in", &[Arg::Str(a)]);
        e.queue_osc("/direct/text", &[Arg::Str(b)]);
        render.push(measure(|| e.fill(&mut out)));
        drained.push(drained_frees(&retired));
    }
    assert!(
        render.iter().all(|c| (c.allocs, c.frees) == (0, 0)),
        "the render thread never allocates or frees a string: {render:?}"
    );
    let mut owed = vec![2; 16];
    owed[0] = 1;
    assert_eq!(drained, owed, "one free per replaced string, and no other");

    // A block with no send hands off nothing, even right after a replacement.
    let after = measure(|| e.fill(&mut out));
    assert_eq!((after.allocs, after.frees), (0, 0), "after the sends");
    assert!(retired.is_empty(), "nothing retired without a send");

    // Two sends to one port in one block: the first is overwritten within the block. Both the
    // previously latched string and the overwritten one are freed at the next drain.
    e.queue_osc("/direct/text", &[Arg::Str("first".into())]);
    e.queue_osc("/direct/text", &[Arg::Str("second".into())]);
    let frees: Vec<usize> = (0..3).map(|_| measure(|| e.fill(&mut out)).frees).collect();
    assert_eq!(
        frees,
        [0, 0, 0],
        "an overwrite within a block frees nothing on render"
    );
    assert_eq!(drained_frees(&retired), 2, "the drain frees both");
    assert_eq!(retired.dropped_full(), 0);
}

/// An Engine with no retire queue (one no Coordinator built) drops what it replaces on the render
/// thread: the fallback the queue exists to remove.
#[test]
fn without_a_retire_queue_the_render_thread_frees_a_replaced_string() {
    const ONE: &str = r#"{ "format_version": 3, "instrument": "one",
      "nodes": [ { "type": "str_len", "address": "/direct", "inputs": { "text": "x" } } ] }"#;
    let mut e = engine(ONE);
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    e.queue_osc("/direct/text", &[Arg::Str("warm".into())]);
    e.fill(&mut out);
    assert_counter_is_live();
    e.queue_osc("/direct/text", &[Arg::Str("next".into())]);
    assert_eq!(
        measure(|| e.fill(&mut out)).frees,
        1,
        "the replaced \"warm\""
    );
}

/// A `str` pipe with two consumers is rendered, not dissolved. Its first block forwards its
/// default onto each consumer's latch, which holds that consumer's own `""` seed: with no send at
/// all, up to one string per consumer is replaced. That happens in the first block of every freshly
/// built Plan, so in every Swap's install window too, and none of it is freed on the render thread.
#[test]
fn a_rendered_str_pipe_retires_each_consumer_seed_in_its_first_block() {
    use reuben_core::coordinator::RenderSlot;
    use reuben_document::coordinator::Coordinator;

    const FAN_OUT: &str = r#"{
      "format_version": 3, "instrument": "fan_out",
      "interface": { "inputs": { "pat": { "type": "str", "default": "seed" } } },
      "nodes": [
        { "type": "str_len", "address": "/a", "inputs": { "text": { "from": "/pat" } } },
        { "type": "str_len", "address": "/b", "inputs": { "text": { "from": "/pat" } } } ] }"#;
    const CONSUMERS: usize = 2;

    // A fresh Plan, measured from its very first block: no warm-up, no send.
    let (mut e, retired) = retiring(engine(FAN_OUT));
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    assert_counter_is_live();
    let first = measure(|| e.fill(&mut out));
    assert_eq!(first.frees, 0, "nothing freed on render");
    assert_eq!(
        drained_frees(&retired),
        CONSUMERS,
        "each consumer's own seed, freed at the drain"
    );
    let next = measure(|| e.fill(&mut out));
    assert_eq!((next.allocs, next.frees), (0, 0), "then nothing");
    // The forwarded copy left in the first block's emit scratch is retired here; the pipe and the
    // consumers still hold that string, so draining it frees nothing.
    assert_eq!(drained_frees(&retired), 0);

    // Each Swap builds a fresh Plan, so its install window repeats it: the Coordinator's Engine
    // retires into the Coordinator's queue, and its reclaim frees the seeds.
    const BLOCK: usize = 128;
    let (mut coord, side, _) = Coordinator::install_initial(
        FAN_OUT,
        registry(),
        Box::new(MemoryResolver::new()),
        AudioConfig::new(48_000.0, BLOCK),
    )
    .expect("initial install");
    let retired = coord.retired();
    let mut slot = RenderSlot::new(side);
    let mut out = vec![0.0f32; BLOCK * slot.channels()];
    for _ in 0..8 {
        slot.fill(&mut out);
    }
    for edit in 0..3 {
        let report = coord.swap_document(FAN_OUT);
        assert!(report.report.ok, "{:?}", report.report);
        let mut saw_ramp = false;
        let mut frees = 0;
        for _ in 0..16 {
            frees += measure(|| slot.fill(&mut out)).frees;
            saw_ramp |= slot.is_ramping();
        }
        assert!(
            saw_ramp && !slot.is_ramping(),
            "the swap installed inside the window"
        );
        assert_eq!(frees, 0, "edit {edit}: nothing freed on render");
        assert!(!retired.is_empty(), "edit {edit}: the case is real");
        assert!(coord.try_reclaim().is_some(), "a retiree came home");
        assert!(
            retired.is_empty(),
            "edit {edit}: the reclaim drained the queue"
        );
    }
    assert_eq!(retired.dropped_full(), 0);
}

/// A child that exposes a `str` output is authored, validated and edited on its own — where the
/// output is inert, with a warning, because a host cannot read a `str` off a played instrument —
/// and works under a parent that wires it through a `subpatch` node.
#[test]
fn a_str_interface_output_is_inert_at_the_root_and_wires_when_nested() {
    use reuben_document::edit::{add_instrument_interface_output, set_instrument_name};
    use reuben_document::introspect::validate;

    let registry = registry();
    const BARE: &str = r#"{ "format_version": 3, "instrument": "child",
      "interface": { "inputs": { "pat": { "type": "str", "default": "inner" } } },
      "nodes": [] }"#;
    let r = resolver_with(BARE);
    let added = add_instrument_interface_output(
        SRC, "o", "/pat.out", None, None, None, None, &registry, &r,
    )
    .expect("runs");
    assert!(added.written, "{:?}", added.report);
    assert!(
        added
            .report
            .warnings
            .iter()
            .any(|w| w.message.contains("`str`")),
        "{:?}",
        added.report
    );
    let renamed = set_instrument_name(SRC, "renamed", &registry, &r).expect("runs");
    assert!(renamed.written, "{:?}", renamed.report);

    let child = {
        use reuben_document::ResourceResolver;
        r.resolve_text(SRC).unwrap()
    };
    let report = validate(&child, &registry, &MemoryResolver::new());
    assert!(report.ok, "{report:?}");
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.port.as_deref() == Some("o") && w.message.contains("subpatch")),
        "{report:?}"
    );

    // At the root, nothing is captured: a host asking for the output gets no slot, not 0.0.
    let graph = load_instrument(&child, &registry, &MemoryResolver::new())
        .expect("loads")
        .graph;
    let plan = Plan::instantiate(graph, AudioConfig::new(48_000.0, 64)).unwrap();
    assert_eq!(plan.interface_value_slot("o"), None);
    assert!(plan.captured.is_empty());

    // Nested, the parent wires it like any port, and no warning reaches the parent.
    let mut r = MemoryResolver::new();
    r.insert_text("child.json", &child);
    const PARENT: &str = r#"{ "format_version": 3, "instrument": "parent",
      "resources": { "child": "child.json" },
      "nodes": [
        { "type": "subpatch", "address": "/sub", "patch": "child" },
        { "type": "str_len", "address": "/n", "inputs": { "text": { "from": "/sub.o" } } },
        { "type": "osc_out", "address": "/len", "inputs": { "in": { "from": "/n.len" } } } ] }"#;
    let loaded = load_instrument(PARENT, &registry, &r).expect("a nested str output wires");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let mut e =
        Engine::new(Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, 64)).unwrap());
    assert_eq!(sent(&block(&mut e), "/len"), [5.0], "\"inner\"");
    e.queue_osc("/sub/pat/in", &[Arg::Str("xy".into())]);
    assert_eq!(sent(&block(&mut e), "/len"), [2.0]);
}

/// A `str` pipe feeding `osc_out` is rendered, and forwards only when its latch is replaced: the
/// seed once, then each send once, and nothing on the blocks between.
#[test]
fn a_rendered_str_pipe_forwards_only_a_replaced_string() {
    const ECHO: &str = r#"{ "format_version": 3, "instrument": "echo",
      "interface": { "inputs": { "pat": { "type": "str", "default": "seed" } } },
      "nodes": [ { "type": "osc_out", "address": "/o", "inputs": { "in": { "from": "/pat" } } } ] }"#;
    let mut e = engine(ECHO);
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    let mut echoed = |e: &mut Engine| -> Vec<Arg> {
        e.fill(&mut out);
        e.drain_outbound().map(|m| m.arg).collect()
    };
    assert_eq!(echoed(&mut e), [Arg::Str("seed".into())]);
    for _ in 0..4 {
        assert!(
            echoed(&mut e).is_empty(),
            "an unchanged latch is not re-sent"
        );
    }
    e.queue_osc("/pat/in", &[Arg::Str("next".into())]);
    assert_eq!(echoed(&mut e), [Arg::Str("next".into())]);
    assert!(echoed(&mut e).is_empty());
    // The same text sent again is a new send, and goes out again.
    e.queue_osc("/pat/in", &[Arg::Str("next".into())]);
    assert_eq!(echoed(&mut e), [Arg::Str("next".into())]);
}

/// A voice patch for the Voicer tests: `gate`/`freq` pipes the Voicer drives, an `active` output,
/// and a `str` pipe fanned out to two consumers, so it is rendered.
fn voice_patch(active_from: &str) -> String {
    format!(
        r#"{{ "format_version": 3, "instrument": "voice",
      "interface": {{ "inputs": {{
          "gate": {{ "type": "f32", "default": 0.0, "min": 0.0, "max": 1.0 }},
          "freq": {{ "type": "f32", "default": 440.0, "min": 20.0, "max": 20000.0 }},
          "pat": {{ "type": "str", "default": "seed" }} }},
        "outputs": {{ "active": {{ "from": "{active_from}" }} }} }},
      "nodes": [
        {{ "type": "envelope", "address": "/env", "inputs": {{ "gate": {{ "from": "/gate" }} }} }},
        {{ "type": "str_len", "address": "/a", "inputs": {{ "text": {{ "from": "/pat" }} }} }},
        {{ "type": "str_len", "address": "/b", "inputs": {{ "text": {{ "from": "/pat" }} }} }} ] }}"#
    )
}

const HOST: &str = r#"{ "format_version": 3, "instrument": "host",
  "resources": { "voice": "voice.json" },
  "interface": { "inputs": { "notes": { "type": "note" } } },
  "nodes": [ { "type": "voicer", "address": "/v", "config": { "voices": 4 }, "voice": "voice",
               "inputs": { "notes": { "from": "/notes" } } } ] }"#;

/// An idle hosted voice is not rendered, so its rendered pipe's first block — and the consumer
/// seeds it replaces — comes at that voice's first note, not at install. The Voicer hands its
/// voices the Engine's retire queue, so those seeds are not freed on the render thread either.
#[test]
fn a_hosted_voice_retires_its_pipe_consumer_seeds_at_its_first_note() {
    const CONSUMERS: usize = 2;
    let mut r = MemoryResolver::new();
    r.insert_text("voice.json", voice_patch("/env.active"));
    let graph = load_instrument(HOST, &registry(), &r).expect("loads").graph;
    let (mut e, retired) = retiring(Engine::new(
        Plan::instantiate(graph, AudioConfig::new(48_000.0, 64)).unwrap(),
    ));
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    assert_counter_is_live();
    let idle: usize = (0..8).map(|_| measure(|| e.fill(&mut out)).frees).sum();
    assert_eq!(idle, 0, "no voice has rendered yet");
    assert!(retired.is_empty());
    // Four held notes take four fresh voices; each one's first rendered block replaces its two
    // consumers' seeds, and the drain frees them.
    for (i, pitch) in [60.0f32, 62.0, 64.0, 65.0].into_iter().enumerate() {
        e.queue_osc("/notes/in", &[Arg::F32(pitch), Arg::F32(1.0)]);
        let frees = measure(|| e.fill(&mut out)).frees;
        assert_eq!(frees, 0, "voice {i}'s first note frees nothing on render");
        assert_eq!(drained_frees(&retired), CONSUMERS, "voice {i}'s seeds");
    }
}

/// The Voicer reads a voice's held `active` output as a number, so any other held type is
/// refused: a `str`, and an enum or a `note` alike. A numeric `i32` loads.
#[test]
fn a_hosted_voice_refuses_a_non_numeric_active_output() {
    let voice_with = |extra: &str, from: &str| {
        format!(
            r#"{{ "format_version": 3, "instrument": "voice",
          "interface": {{ "inputs": {{
              "gate": {{ "type": "f32", "default": 0.0, "min": 0.0, "max": 1.0 }},
              "freq": {{ "type": "f32", "default": 440.0, "min": 20.0, "max": 20000.0 }},
              {extra} }},
            "outputs": {{ "active": {{ "from": "{from}" }} }} }},
          "nodes": [] }}"#
        )
    };
    let refused = [
        ("str", voice_patch("/pat.out")),
        (
            "enum",
            voice_with(
                r#""mode": { "type": "FilterMode", "default": "Hp" }"#,
                "/mode.out",
            ),
        ),
        ("note", voice_with(r#""nn": { "type": "note" }"#, "/nn.out")),
    ];
    for (label, voice) in refused {
        let mut r = MemoryResolver::new();
        r.insert_text("voice.json", voice);
        let errs: Vec<LoadError> = match load_instrument(HOST, &registry(), &r) {
            Ok(_) => panic!("{label}: a non-numeric `active` should be refused"),
            Err(errs) => errs.into_iter().collect(),
        };
        let [LoadError::Nested { node, error }] = errs.as_slice() else {
            panic!("{label}: one nested error: {errs:?}");
        };
        assert_eq!(node, "/v", "{label}");
        assert!(
            matches!(&**error, LoadError::InterfacePipe { name, .. } if name == "active"),
            "{label}: {error:?}"
        );
    }

    let mut r = MemoryResolver::new();
    r.insert_text(
        "voice.json",
        voice_with(
            r#""cnt": { "type": "i32", "default": 1, "min": 0, "max": 4 }"#,
            "/cnt.out",
        ),
    );
    load_instrument(HOST, &registry(), &r).expect("an i32 `active` loads");
}

/// Validated on its own, a voice patch's `str` `active` warns that a Voicer refuses it, rather
/// than offering the `subpatch` route that suits other names.
#[test]
fn a_str_active_validated_alone_says_a_voicer_refuses_it() {
    let report = reuben_document::introspect::validate(
        &voice_patch("/pat.out"),
        &registry(),
        &MemoryResolver::new(),
    );
    assert!(report.ok, "{report:?}");
    let warning = report
        .warnings
        .iter()
        .find(|w| w.port.as_deref() == Some("active"))
        .unwrap_or_else(|| panic!("{report:?}"));
    assert!(warning.message.contains("Voicer"), "{}", warning.message);
}

/// An interface output reading a single-consumer `str` pipe keeps the pipe rendered, so its first
/// block replaces the consumer's seed, which the drain frees; without the output the pipe
/// dissolves and nothing is replaced.
#[test]
fn an_interface_output_keeps_a_str_pipe_rendered() {
    const WITH: &str = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str", "default": "seed" } },
                     "outputs": { "o": { "from": "/pat.out" } } },
      "nodes": [ { "type": "str_len", "address": "/a", "inputs": { "text": { "from": "/pat" } } } ] }"#;
    let without = WITH.replace(
        r#",
                     "outputs": { "o": { "from": "/pat.out" } }"#,
        "",
    );
    assert_ne!(without, WITH);
    assert_counter_is_live();
    let first_block = |doc: &str| {
        let (mut e, retired) = retiring(engine(doc));
        let mut out = vec![0.0f32; e.block_size() * e.channels()];
        let render = measure(|| e.fill(&mut out)).frees;
        (render, drained_frees(&retired))
    };
    assert_eq!(first_block(WITH), (0, 1), "rendered: the consumer's seed");
    assert_eq!(
        first_block(&without),
        (0, 0),
        "dissolved: nothing to overwrite"
    );
}
