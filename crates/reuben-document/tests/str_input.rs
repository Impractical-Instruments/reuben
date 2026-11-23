//! A `str` input end to end: an Operator declaring one with its preparer, fed a document default
//! through a `str` interface pipe, a node literal, and live sends of prepared values; the
//! render-side refusal of raw text and of another preparer's values; a document default that does
//! not compile; the load-time wire check; the save round-trip; the authoring verbs; the routing
//! table; and what the render thread allocates and frees while prepared values change under it.
//!
//! No built-in Operator prepares its text, so `steps` and `words` (`prepared/mod.rs`) are
//! registered here.

mod prepared;
mod rt_alloc;

use rt_alloc::{assert_counter_is_live, measure, Counting};

use prepared::{registry, steps, steps_arg, words_arg};
use reuben_core::coordinator::{retire_queue, RetireDrain};
use reuben_core::message::{Arg, Message};
use reuben_core::plan::{Plan, PlanError, PreparedInput};
use reuben_core::{AudioConfig, Engine};
use reuben_document::coordinator::RETIRE_QUEUE_CAPACITY;
use reuben_document::edit::{add_instrument_interface_input, set_instrument_input};
use reuben_document::{load_instrument, LoadError, MemoryResolver, NormalizedDoc};
use serde_json::json;

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn the_contract_declares_a_prepared_str_port() {
    use reuben_core::descriptor::PortType;
    use reuben_core::operator::Operator;
    let d = <steps::Steps as Operator>::descriptor();
    let PortType::Str {
        preparer: Some(preparer),
    } = &d.inputs[steps::IN_STEPS.index()].ty
    else {
        panic!("a prepared str port: {:?}", d.inputs[0].ty);
    };
    assert!(
        preparer.kind.ends_with("::parse_steps"),
        "the kind names the preparer: {}",
        preparer.kind
    );
    assert_ne!(*preparer, prepared::words_preparer());
}

/// `/pat` is a `str` pipe declaring a default, feeding `/from_pipe`; `/literal` carries a node
/// literal; `/unset` carries nothing. Each reports its sum to its own `osc_out` sink.
const DOC: &str = r#"{
  "format_version": 3,
  "instrument": "str_input",
  "interface": { "inputs": { "pat": { "type": "str", "default": "1 2 3" } } },
  "nodes": [
    { "type": "steps", "address": "/from_pipe", "inputs": { "steps": { "from": "/pat" } } },
    { "type": "steps", "address": "/literal", "inputs": { "steps": "4 5" } },
    { "type": "steps", "address": "/unset" },
    { "type": "osc_out", "address": "/pipe_sum", "inputs": { "in": { "from": "/from_pipe.total" } } },
    { "type": "osc_out", "address": "/literal_sum", "inputs": { "in": { "from": "/literal.total" } } },
    { "type": "osc_out", "address": "/unset_sum", "inputs": { "in": { "from": "/unset.total" } } }
  ]
}"#;

fn plan(doc: &str) -> Plan {
    let graph = load_instrument(doc, &registry(), &MemoryResolver::new())
        .expect("loads")
        .graph;
    Plan::instantiate(graph, AudioConfig::new(48_000.0, 64)).expect("instantiates")
}

fn engine(doc: &str) -> Engine {
    Engine::new(plan(doc))
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
        .map(|m| (m.address, m.arg.as_f32().expect("a sum")))
        .collect()
}

/// An outbound arg as native puts it on the wire: a prepared value leaves as its text.
fn external(arg: Arg) -> Arg {
    let mut flat = Vec::new();
    assert!(reuben_core::boundary::osc_out_args(&arg, &mut flat));
    let [one] = <[Arg; 1]>::try_from(flat).expect("one atom");
    one
}

fn sent(pairs: &[(String, f32)], sink: &str) -> Vec<f32> {
    pairs
        .iter()
        .filter(|(a, _)| a == sink)
        .map(|(_, v)| *v)
        .collect()
}

#[test]
fn a_str_input_reads_its_prepared_document_default_then_each_prepared_send() {
    let mut e = engine(DOC);
    let first = block(&mut e);
    // Each default is compiled at Plan build; an unset input holds the compile of "".
    assert_eq!(sent(&first, "/pipe_sum"), [6.0]);
    assert_eq!(sent(&first, "/literal_sum"), [9.0]);
    assert_eq!(sent(&first, "/unset_sum"), [0.0]);

    // Through the pipe's address, and straight onto a node input.
    e.queue_osc("/pat/in", &[steps_arg("10 20")]);
    e.queue_osc("/literal/steps", &[steps_arg("7")]);
    let live = block(&mut e);
    assert_eq!(sent(&live, "/pipe_sum"), [30.0]);
    assert_eq!(sent(&live, "/literal_sum"), [7.0]);

    // Held: nothing new, nothing re-sent.
    assert!(block(&mut e).is_empty());
    assert_eq!(e.take_dropped_unprepared(), 0);
}

/// Raw text skipped the producer's prepare, so the input cannot read it: dropped and counted, by
/// every route a send takes, and the input keeps what it held.
#[test]
fn a_raw_string_at_a_preparing_input_is_dropped_and_counted() {
    let mut e = engine(DOC);
    block(&mut e);
    e.queue_osc("/pat/in", &[Arg::Str("10 20".into())]);
    e.queue_osc("/literal/steps", &[Arg::Str("7".into())]);
    e.queue(Message::new("/unset/steps", Arg::Str("1".into()), 0));
    assert!(block(&mut e).is_empty(), "no input changed");
    assert_eq!(e.take_dropped_unprepared(), 3);
    assert_eq!(e.take_dropped_unprepared(), 0, "the take leaves zero");

    // A prepared send still lands after the refusals.
    e.queue_osc("/literal/steps", &[steps_arg("2 2")]);
    assert_eq!(sent(&block(&mut e), "/literal_sum"), [4.0]);
}

/// A value another preparer built is not this input's payload: dropped and counted.
#[test]
fn a_prepared_value_of_another_kind_is_dropped_and_counted() {
    let mut e = engine(DOC);
    block(&mut e);
    e.queue_osc("/pat/in", &[words_arg("ab cd")]);
    e.queue_osc("/literal/steps", &[words_arg("ab")]);
    assert!(block(&mut e).is_empty());
    assert_eq!(e.take_dropped_unprepared(), 2);
}

#[test]
fn a_number_never_lands_on_a_str_input() {
    let mut e = engine(DOC);
    block(&mut e);
    // The flat-arg boundary refuses it outright.
    e.queue_osc("/pat/in", &[Arg::F32(3.0)]);
    e.queue_osc("/literal/steps", &[Arg::I32(3)]);
    assert!(block(&mut e).is_empty());
    // A built Message skips that check; the port's own latch rule refuses it.
    e.queue(Message::new("/literal/steps", Arg::F32(3.0), 0));
    e.queue(Message::new("/pat/in", Arg::F32(3.0), 0));
    assert!(block(&mut e).is_empty());
    assert_eq!(
        e.take_dropped_unprepared(),
        0,
        "a number is not a string to count"
    );
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
      "nodes": [ { "type": "steps", "address": "/n", "inputs": { "steps": { "from": "/x" } } } ] }"#;
    let errs = load_err(f32_into_str);
    assert!(
        errs.iter().any(|e| e.to_string().contains("Str")),
        "{errs:?}"
    );

    // Across a subpatch boundary too: the face carries the child pipe's `str` type.
    const CHILD: &str = r#"{ "format_version": 3, "instrument": "c",
      "interface": { "inputs": { "pat": { "type": "str" } } },
      "nodes": [ { "type": "steps", "address": "/n", "inputs": { "steps": { "from": "/pat" } } } ] }"#;
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
}

/// `osc_out`'s pass-through takes any type with an OSC form, a `str` pipe included. Beside a
/// preparing input the pipe forwards the prepared value, which leaves as the text it came from; with
/// no preparing input the pipe holds nothing a send can set, so it sends nothing.
#[test]
fn a_str_pipe_echoes_through_osc_out_only_beside_a_preparing_input() {
    let echoed = |doc: &str, send: Arg| {
        let mut e = engine(doc);
        let mut out = vec![0.0f32; e.block_size() * e.channels()];
        e.fill(&mut out);
        let seed: Vec<Arg> = e.drain_outbound().map(|m| external(m.arg)).collect();
        e.queue_osc("/pat/in", &[send]);
        e.fill(&mut out);
        let after: Vec<Arg> = e.drain_outbound().map(|m| external(m.arg)).collect();
        (seed, after, e.take_dropped_unprepared())
    };
    const BESIDE: &str = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str", "default": "1" } } },
      "nodes": [ { "type": "osc_out", "address": "/o", "inputs": { "in": { "from": "/pat" } } },
                 { "type": "steps", "address": "/n", "inputs": { "steps": { "from": "/pat" } } } ] }"#;
    let (seed, after, dropped) = echoed(BESIDE, steps_arg("2 3"));
    assert_eq!(seed, [Arg::Str("1".into())]);
    assert_eq!(after, [Arg::Str("2 3".into())]);
    assert_eq!(dropped, 0);

    const ALONE: &str = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str", "default": "1" } } },
      "nodes": [ { "type": "osc_out", "address": "/o", "inputs": { "in": { "from": "/pat" } } } ] }"#;
    let (seed, after, dropped) = echoed(ALONE, Arg::Str("echo".into()));
    assert!(seed.is_empty() && after.is_empty(), "{seed:?} {after:?}");
    assert_eq!(dropped, 1, "the raw string is refused at the pipe");
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
        json!({ "type": "str", "default": "1 2 3" })
    );
    let literal = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["address"] == "/literal")
        .expect("/literal");
    assert_eq!(literal["inputs"]["steps"], json!("4 5"));

    // And what was saved loads back to the same values.
    let again = serde_json::to_string(&v).unwrap();
    let mut e = engine(&again);
    let first = block(&mut e);
    assert_eq!(sent(&first, "/pipe_sum"), [6.0]);
    assert_eq!(sent(&first, "/literal_sum"), [9.0]);
}

/// A document default that does not compile is refused at Plan build, at the input holding it,
/// with the byte span its preparer named: a node literal, and an interface input's default.
#[test]
fn a_document_default_that_does_not_compile_fails_to_plan_with_its_span() {
    let failures = |doc: &str| {
        let graph = load_instrument(doc, &registry(), &MemoryResolver::new())
            .expect("the text is a well-typed str")
            .graph;
        match Plan::instantiate(graph, AudioConfig::new(48_000.0, 64)) {
            Err(PlanError::Prepare { failures }) => failures
                .into_iter()
                .map(|f| (f.node, f.port, f.error.span))
                .collect::<Vec<_>>(),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("a value that does not compile fails to plan"),
        }
    };
    let bad_literal = DOC.replace(r#""steps": "4 5""#, r#""steps": "4 x5""#);
    assert_eq!(
        failures(&bad_literal),
        [("/literal".into(), "steps".into(), 2..4)]
    );
    let bad_default = DOC.replace(r#""default": "1 2 3""#, r#""default": "1 2 ?""#);
    assert_eq!(failures(&bad_default), [("/pat".into(), "in".into(), 4..5)]);

    // Every refusal at once, not the first.
    let both = bad_literal.replace(r#""default": "1 2 3""#, r#""default": "1 2 ?""#);
    let mut got = failures(&both);
    got.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        got,
        [
            ("/literal".into(), "steps".into(), 2..4),
            ("/pat".into(), "in".into(), 4..5)
        ]
    );
    let report = reuben_document::introspect::validate(&both, &registry(), &MemoryResolver::new());
    assert_eq!(report.errors.len(), 2, "{report:?}");
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
        set_instrument_input(SRC, "/unset", "steps", json!("8 9"), &registry, &r).expect("runs");
    assert!(done.written, "{:?}", done.report);
    let done =
        set_instrument_input(SRC, "/pat", "in", json!("2 4 6"), &registry, &r).expect("runs");
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
    assert_eq!(unset["inputs"]["steps"], json!("8 9"));
    assert_eq!(v["interface"]["inputs"]["pat"]["default"], json!("2 4 6"));
    assert_eq!(
        v["interface"]["inputs"]["words"],
        json!({ "type": "str", "default": "one two" })
    );

    // A number is refused on a `str` input and a `str` pipe, and nothing is written.
    for refused in [
        set_instrument_input(SRC, "/unset", "steps", json!(3), &registry, &r),
        set_instrument_input(SRC, "/pat", "in", json!(3), &registry, &r),
    ] {
        let refused = refused.expect("runs");
        assert!(!refused.written, "{:?}", refused.report);
    }
    assert_eq!(readback(&r), v);
}

/// An edit whose text does not compile is refused like any invalid edit, nothing written, and the
/// refusal names the input and the byte span.
#[test]
fn an_edit_whose_text_does_not_compile_is_refused_with_its_span() {
    let registry = registry();
    let r = resolver_with(DOC);
    let before = readback(&r);
    for (node, port, text, span) in [
        ("/literal", "steps", "1 two", "bytes 2..5"),
        ("/pat", "in", "x", "bytes 0..1"),
    ] {
        let refused =
            set_instrument_input(SRC, node, port, json!(text), &registry, &r).expect("runs");
        assert!(!refused.written, "{node}: {:?}", refused.report);
        let [error] = refused.report.errors.as_slice() else {
            panic!("{node}: one error: {:?}", refused.report);
        };
        assert_eq!(error.node.as_deref(), Some(node));
        assert_eq!(error.port.as_deref(), Some(port));
        assert!(error.message.contains(span), "{}", error.message);
        assert!(error.message.contains("whole number"), "{}", error.message);
    }
    assert_eq!(readback(&r), before);
}

/// A kept interface `str` pipe forwards one prepared value to every input it feeds, so inputs with
/// different preparers cannot share one: the document is refused. A dissolved pipe feeds one input
/// and never conflicts.
#[test]
fn a_kept_str_pipe_feeding_two_kinds_of_preparer_is_refused() {
    const MIXED: &str = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str", "default": "" } } },
      "nodes": [ { "type": "steps", "address": "/s", "inputs": { "steps": { "from": "/pat" } } },
                 { "type": "words", "address": "/w", "inputs": { "text": { "from": "/pat" } } } ] }"#;
    let graph = load_instrument(MIXED, &registry(), &MemoryResolver::new())
        .expect("the wires are well-typed")
        .graph;
    let Err(PlanError::PreparerConflict { pipe, .. }) =
        Plan::instantiate(graph, AudioConfig::new(48_000.0, 64))
    else {
        panic!("two preparers behind one kept pipe");
    };
    assert_eq!(pipe, "/pat");
    let report = reuben_document::introspect::validate(MIXED, &registry(), &MemoryResolver::new());
    assert!(!report.ok);
    assert_eq!(report.errors[0].node.as_deref(), Some("/pat"));
}

/// A send to a kept `str` pipe in front of preparing inputs arrives prepared: the pipe took its
/// consumers' preparer, latches the prepared value, and forwards it to each.
#[test]
fn a_send_to_a_kept_str_pipe_arrives_prepared() {
    const FAN_OUT: &str = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str", "default": "1" } } },
      "nodes": [
        { "type": "steps", "address": "/a", "inputs": { "steps": { "from": "/pat" } } },
        { "type": "steps", "address": "/b", "inputs": { "steps": { "from": "/pat" } } },
        { "type": "osc_out", "address": "/a_sum", "inputs": { "in": { "from": "/a.total" } } },
        { "type": "osc_out", "address": "/b_sum", "inputs": { "in": { "from": "/b.total" } } } ] }"#;
    let p = plan(FAN_OUT);
    let pipe = p
        .nodes()
        .iter()
        .find(|n| &*n.address == "/pat")
        .expect("a fan-out pipe is kept");
    assert_eq!(
        pipe.descriptor.inputs[0].preparer(),
        Some(&prepared::steps_preparer()),
        "the pipe takes its consumers' preparer"
    );
    let mut e = Engine::new(p);
    let first = block(&mut e);
    assert_eq!(
        (sent(&first, "/a_sum"), sent(&first, "/b_sum")),
        (vec![1.0], vec![1.0])
    );
    e.queue_osc("/pat/in", &[steps_arg("4 4")]);
    let live = block(&mut e);
    assert_eq!(
        (sent(&live, "/a_sum"), sent(&live, "/b_sum")),
        (vec![8.0], vec![8.0])
    );
    e.queue_osc("/pat/in", &[Arg::Str("5".into())]);
    assert!(block(&mut e).is_empty());
    assert_eq!(e.take_dropped_unprepared(), 1);
}

/// The routing table names each address whose sends are prepared, with the preparer and the inputs
/// a send there reaches: a dissolved pipe through its alias, a kept pipe through its forward
/// (across a nested child's pipe too), and a node's own unwired input. A wired input takes no send,
/// so it has no route.
#[test]
fn the_routing_table_follows_aliases_and_kept_pipes() {
    const CHILD: &str = r#"{ "format_version": 3, "instrument": "c",
      "interface": { "inputs": { "inner": { "type": "str" } } },
      "nodes": [ { "type": "words", "address": "/w", "inputs": { "text": { "from": "/inner" } } } ] }"#;
    const DOC: &str = r#"{ "format_version": 3, "instrument": "t", "resources": { "c": "c.json" },
      "interface": { "inputs": {
          "one": { "type": "str" },
          "fan": { "type": "str" },
          "nested": { "type": "str" } } },
      "nodes": [
        { "type": "steps", "address": "/alone", "inputs": { "steps": { "from": "/one" } } },
        { "type": "steps", "address": "/f1", "inputs": { "steps": { "from": "/fan" } } },
        { "type": "steps", "address": "/f2", "inputs": { "steps": { "from": "/fan" } } },
        { "type": "steps", "address": "/own" },
        { "type": "subpatch", "address": "/sub", "patch": "c",
          "inputs": { "inner": { "from": "/nested" } } } ] }"#;
    let mut r = MemoryResolver::new();
    r.insert_text("c.json", CHILD);
    let graph = load_instrument(DOC, &registry(), &r).expect("loads").graph;
    let p = Plan::instantiate(graph, AudioConfig::new(48_000.0, 64)).expect("instantiates");
    let routes = p.prepared_routes();
    let input = |node: &str, port: &'static str| PreparedInput {
        node: node.into(),
        port,
    };
    let at = |address: &str| {
        routes
            .get(address)
            .unwrap_or_else(|| panic!("{address}: {routes:?}"))
    };

    let alone = at("/one/in");
    assert_eq!(alone.preparer, prepared::steps_preparer());
    assert_eq!(alone.inputs, [input("/alone", "steps")]);

    let mut fan = at("/fan/in").inputs.clone();
    fan.sort_by(|a, b| a.node.cmp(&b.node));
    assert_eq!(fan, [input("/f1", "steps"), input("/f2", "steps")]);

    let nested = at("/nested/in");
    assert_eq!(nested.preparer, prepared::words_preparer());
    assert_eq!(nested.inputs, [input("/sub/w", "text")]);

    assert_eq!(at("/own/steps").inputs, [input("/own", "steps")]);
    for wired in ["/alone/steps", "/f1/steps", "/sub/w/text"] {
        assert!(routes.get(wired).is_none(), "{wired} is fed by a pipe");
    }
    let addresses: Vec<&str> = routes.iter().map(|r| r.address.as_str()).collect();
    assert!(!addresses.contains(&"/nowhere"), "{addresses:?}");
    // The table is shared, not copied: a producer holds its own handle.
    let held = std::sync::Arc::clone(routes);
    drop(p);
    assert!(held.get("/one/in").is_some());
}

/// The render thread neither allocates nor frees a prepared value. Each value it lets go of while
/// they change under it — a latch's seed, an authored value, a value a send replaced, its own
/// copies in the control queue and routing scratch — goes to the retire queue, and what was last
/// held there is freed where the queue is drained.
#[test]
fn changing_prepared_values_allocate_and_free_nothing_on_the_render_thread() {
    // No `osc_out` sinks: each outbound send builds a Message on the render thread, which is that
    // sink's own documented cost, not a prepared value's.
    const QUIET: &str = r#"{
      "format_version": 3, "instrument": "quiet",
      "interface": { "inputs": { "pat": { "type": "str", "default": "1" } } },
      "nodes": [
        { "type": "steps", "address": "/from_pipe", "inputs": { "steps": { "from": "/pat" } } },
        { "type": "steps", "address": "/direct", "inputs": { "steps": "2" } },
        { "type": "steps", "address": "/unset" } ] }"#;
    let (mut e, retired) = retiring(engine(QUIET));
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    for _ in 0..4 {
        e.fill(&mut out);
    }
    assert_counter_is_live();

    let idle = measure(|| {
        for _ in 0..16 {
            e.fill(&mut out);
        }
    });
    assert_eq!((idle.allocs, idle.frees), (0, 0), "idle blocks");

    // The first send to each input, one input per block: an unset input's compiled `""`, a node
    // literal and a pipe default are each replaced, and retired rather than freed. The first two
    // are held by nothing but their latch, so the drain frees them; the dissolved pipe's address
    // keeps its own handle on its default.
    for (addr, last_holder) in [
        ("/unset/steps", true),
        ("/direct/steps", true),
        ("/pat/in", false),
    ] {
        let send = [steps_arg("3 4")];
        let render = measure(|| {
            e.queue_osc(addr, &send);
            e.fill(&mut out);
        });
        assert_eq!(
            (render.allocs, render.frees),
            (0, 0),
            "first send to {addr}"
        );
        let drained = drained_frees(&retired);
        assert_eq!(
            drained > 0,
            last_holder,
            "{addr}: {drained} free(s) at the drain"
        );
    }

    // Repeated sends to the same two inputs, each value held by nothing but the engine once
    // queued: the render thread frees nothing, and every drain frees what that block replaced.
    for i in 0..16u32 {
        let args = [steps_arg(&format!("{i} {i}")), steps_arg(&format!("{i}"))];
        let render = measure(|| {
            e.queue_osc("/pat/in", &args[..1]);
            e.queue_osc("/direct/steps", &args[1..]);
            e.fill(&mut out);
        });
        drop(args);
        assert_eq!((render.allocs, render.frees), (0, 0), "send {i}");
        assert!(drained_frees(&retired) > 0, "send {i}: freed at the drain");
    }

    // A block with no send hands off nothing.
    let after = measure(|| e.fill(&mut out));
    assert_eq!((after.allocs, after.frees), (0, 0), "after the sends");
    assert!(retired.is_empty(), "nothing retired without a send");

    // A raw string refused at the input is retired from the control queue, not freed here.
    let raw = [Arg::Str("9".into())];
    let refused = measure(|| {
        e.queue_osc("/direct/steps", &raw);
        e.fill(&mut out);
    });
    drop(raw);
    assert_eq!(
        (refused.allocs, refused.frees),
        (0, 0),
        "a refused raw string"
    );
    assert_eq!(drained_frees(&retired), 1, "freed at the drain");
    assert_eq!(retired.dropped_full(), 0);
}

/// An Engine with no retire queue (one no Coordinator built) drops what it replaces on the render
/// thread: the fallback the queue exists to remove.
#[test]
fn without_a_retire_queue_the_render_thread_frees_a_replaced_value() {
    const ONE: &str = r#"{ "format_version": 3, "instrument": "one",
      "nodes": [ { "type": "steps", "address": "/direct", "inputs": { "steps": "1" } } ] }"#;
    let mut e = engine(ONE);
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    e.queue_osc("/direct/steps", &[steps_arg("2")]);
    e.fill(&mut out);
    assert_counter_is_live();
    let next = [steps_arg("3")];
    let frees = measure(|| {
        e.queue_osc("/direct/steps", &next);
        e.fill(&mut out);
    })
    .frees;
    assert!(
        frees > 0,
        "the replaced value is freed on the render thread"
    );
}

/// A `str` pipe with two consumers is rendered, not dissolved. Its first block forwards its
/// prepared default onto each consumer's latch. A fed consumer's latch holds a placeholder, not a
/// compiled value of its own, so that first forward frees nothing anywhere. It happens in the first
/// block of every freshly built Plan, so in every Swap's install window too.
#[test]
fn a_rendered_str_pipe_forwards_over_placeholders_in_its_first_block() {
    use reuben_core::coordinator::RenderSlot;
    use reuben_document::coordinator::Coordinator;

    const FAN_OUT: &str = r#"{
      "format_version": 3, "instrument": "fan_out",
      "interface": { "inputs": { "pat": { "type": "str", "default": "1 2" } } },
      "nodes": [
        { "type": "steps", "address": "/a", "inputs": { "steps": { "from": "/pat" } } },
        { "type": "steps", "address": "/b", "inputs": { "steps": { "from": "/pat" } } } ] }"#;

    let (mut e, retired) = retiring(engine(FAN_OUT));
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    assert_counter_is_live();
    let first = measure(|| e.fill(&mut out));
    assert_eq!(first.frees, 0, "nothing freed on render");
    assert_eq!(
        drained_frees(&retired),
        0,
        "the consumers' placeholders own nothing"
    );
    let next = measure(|| e.fill(&mut out));
    assert_eq!((next.allocs, next.frees), (0, 0), "then nothing");
    // The forwarded copy left in the first block's emit scratch is retired here; the pipe and the
    // consumers still hold that value, so draining it frees nothing.
    assert_eq!(drained_frees(&retired), 0);

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
        assert!(
            !retired.is_empty(),
            "edit {edit}: the forwarded copies were retired"
        );
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
/// and works under a parent that wires it through a `subpatch` node into a preparing input.
#[test]
fn a_str_interface_output_is_inert_at_the_root_and_wires_when_nested() {
    use reuben_document::edit::{add_instrument_interface_output, set_instrument_name};
    use reuben_document::introspect::validate;

    let registry = registry();
    const BARE: &str = r#"{ "format_version": 3, "instrument": "child",
      "interface": { "inputs": { "pat": { "type": "str", "default": "5" } } },
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

    // Nested, the parent wires it like any port, and the child's pipe takes the preparer of the
    // parent input it ends up feeding.
    let mut r = MemoryResolver::new();
    r.insert_text("child.json", &child);
    const PARENT: &str = r#"{ "format_version": 3, "instrument": "parent",
      "resources": { "child": "child.json" },
      "nodes": [
        { "type": "subpatch", "address": "/sub", "patch": "child" },
        { "type": "steps", "address": "/n", "inputs": { "steps": { "from": "/sub.o" } } },
        { "type": "osc_out", "address": "/sum", "inputs": { "in": { "from": "/n.total" } } } ] }"#;
    let loaded = load_instrument(PARENT, &registry, &r).expect("a nested str output wires");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let mut e =
        Engine::new(Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, 64)).unwrap());
    assert_eq!(sent(&block(&mut e), "/sum"), [5.0]);
    e.queue_osc("/sub/pat/in", &[steps_arg("1 1")]);
    assert_eq!(sent(&block(&mut e), "/sum"), [2.0]);
}

/// A kept `str` pipe forwards only when its latch is replaced: the seed once, then each send once,
/// and nothing on the blocks between. Each send is a new allocation, so the same text sent again
/// forwards again.
#[test]
fn a_rendered_str_pipe_forwards_only_a_replaced_value() {
    const ECHO: &str = r#"{ "format_version": 3, "instrument": "echo",
      "interface": { "inputs": { "pat": { "type": "str", "default": "1" } } },
      "nodes": [ { "type": "osc_out", "address": "/o", "inputs": { "in": { "from": "/pat" } } },
                 { "type": "steps", "address": "/n", "inputs": { "steps": { "from": "/pat" } } } ] }"#;
    let mut e = engine(ECHO);
    let mut out = vec![0.0f32; e.block_size() * e.channels()];
    let mut echoed = |e: &mut Engine| -> Vec<Arg> {
        e.fill(&mut out);
        e.drain_outbound().map(|m| external(m.arg)).collect()
    };
    assert_eq!(echoed(&mut e), [Arg::Str("1".into())]);
    for _ in 0..4 {
        assert!(
            echoed(&mut e).is_empty(),
            "an unchanged latch is not re-sent"
        );
    }
    e.queue_osc("/pat/in", &[steps_arg("2")]);
    assert_eq!(echoed(&mut e), [Arg::Str("2".into())]);
    assert!(echoed(&mut e).is_empty());
    e.queue_osc("/pat/in", &[steps_arg("2")]);
    assert_eq!(echoed(&mut e), [Arg::Str("2".into())]);
}

/// A voice patch for the Voicer tests: `gate`/`freq` pipes the Voicer drives, an `active` output,
/// and a `str` pipe fanned out to two consumers, so it is rendered.
fn voice_patch(active_from: &str) -> String {
    format!(
        r#"{{ "format_version": 3, "instrument": "voice",
      "interface": {{ "inputs": {{
          "gate": {{ "type": "f32", "default": 0.0, "min": 0.0, "max": 1.0 }},
          "freq": {{ "type": "f32", "default": 440.0, "min": 20.0, "max": 20000.0 }},
          "pat": {{ "type": "str", "default": "1" }} }},
        "outputs": {{ "active": {{ "from": "{active_from}" }} }} }},
      "nodes": [
        {{ "type": "envelope", "address": "/env", "inputs": {{ "gate": {{ "from": "/gate" }} }} }},
        {{ "type": "steps", "address": "/a", "inputs": {{ "steps": {{ "from": "/pat" }} }} }},
        {{ "type": "steps", "address": "/b", "inputs": {{ "steps": {{ "from": "/pat" }} }} }} ] }}"#
    )
}

const HOST: &str = r#"{ "format_version": 3, "instrument": "host",
  "resources": { "voice": "voice.json" },
  "interface": { "inputs": { "notes": { "type": "note" } } },
  "nodes": [ { "type": "voicer", "address": "/v", "config": { "voices": 4 }, "voice": "voice",
               "inputs": { "notes": { "from": "/notes" } } } ] }"#;

/// An idle hosted voice is not rendered, so its rendered pipe's first block comes at that voice's
/// first note, not at install. It forwards over its consumers' placeholders, which own nothing, so
/// it frees nothing on the render thread or at the drain.
#[test]
fn a_hosted_voice_pipe_first_block_frees_nothing() {
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
    for (i, pitch) in [60.0f32, 62.0, 64.0, 65.0].into_iter().enumerate() {
        e.queue_osc("/notes/in", &[Arg::F32(pitch), Arg::F32(1.0)]);
        let frees = measure(|| e.fill(&mut out)).frees;
        assert_eq!(frees, 0, "voice {i}'s first note frees nothing on render");
        assert_eq!(drained_frees(&retired), 0, "voice {i}: nothing to free");
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

/// An interface output reading a single-consumer `str` pipe keeps the pipe rendered; without the
/// output the pipe dissolves. Either way the consumer's first value costs nothing in its first
/// block: rendered, the pipe forwards over the consumer's placeholder; dissolved, the consumer
/// holds the pipe's prepared default from the start.
#[test]
fn an_interface_output_keeps_a_str_pipe_rendered() {
    const WITH: &str = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str", "default": "1" } },
                     "outputs": { "o": { "from": "/pat.out" } } },
      "nodes": [ { "type": "steps", "address": "/a", "inputs": { "steps": { "from": "/pat" } } } ] }"#;
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
    let kept = |doc: &str| plan(doc).nodes().iter().any(|n| &*n.address == "/pat");
    assert!(kept(WITH) && !kept(&without));
    assert_eq!(first_block(WITH), (0, 0), "rendered: nothing to free");
    assert_eq!(
        first_block(&without),
        (0, 0),
        "dissolved: nothing to overwrite"
    );
}

/// Across a Swap, a surviving node keeps the prepared value last sent to its `str` input while the
/// edit leaves that input's document value alone, and the freshly prepared default wins where the
/// edit changed it: a node literal and an interface input's default alike.
#[test]
fn a_survivor_keeps_its_prepared_value_unless_the_edit_changed_its_default() {
    use reuben_core::coordinator::RenderSlot;
    use reuben_document::coordinator::Coordinator;

    const BLOCK: usize = 128;
    let (mut coord, side, _) = Coordinator::install_initial(
        DOC,
        registry(),
        Box::new(MemoryResolver::new()),
        AudioConfig::new(48_000.0, BLOCK),
    )
    .expect("initial install");
    let mut slot = RenderSlot::new(side);
    let mut out = vec![0.0f32; BLOCK * slot.channels()];
    let mut run = |slot: &mut RenderSlot, blocks: usize| -> Vec<(String, f32)> {
        let mut got = Vec::new();
        for _ in 0..blocks {
            slot.fill(&mut out);
            got.extend(
                slot.drain_outbound()
                    .map(|m| (m.address, m.arg.as_f32().expect("a sum"))),
            );
        }
        got
    };
    run(&mut slot, 2);
    slot.queue_osc("/literal/steps", &[steps_arg("10 10")]);
    slot.queue_osc("/pat/in", &[steps_arg("30")]);
    let live = run(&mut slot, 1);
    assert_eq!(
        (sent(&live, "/literal_sum"), sent(&live, "/pipe_sum")),
        (vec![20.0], vec![30.0])
    );

    // An edit elsewhere: both inputs keep what was sent, so nothing re-reports.
    let elsewhere = DOC.replace(
        r#""address": "/unset" }"#,
        r#""address": "/unset", "inputs": { "steps": "1" } }"#,
    );
    assert_ne!(elsewhere, DOC);
    assert!(coord.swap_document(&elsewhere).report.ok);
    let after = run(&mut slot, 16);
    assert!(!slot.is_ramping());
    assert_eq!(
        sent(&after, "/unset_sum"),
        [1.0],
        "the edited input takes its new default"
    );
    assert!(sent(&after, "/literal_sum").is_empty(), "{after:?}");
    assert!(sent(&after, "/pipe_sum").is_empty(), "{after:?}");
    assert!(coord.try_reclaim().is_some());

    // An edit to both document values: each freshly prepared default wins.
    let changed = elsewhere
        .replace(r#""steps": "4 5""#, r#""steps": "6""#)
        .replace(r#""default": "1 2 3""#, r#""default": "2""#);
    assert!(coord.swap_document(&changed).report.ok);
    let after = run(&mut slot, 16);
    assert_eq!(sent(&after, "/literal_sum"), [6.0], "{after:?}");
    assert_eq!(sent(&after, "/pipe_sum"), [2.0], "{after:?}");
    assert_eq!(slot.take_dropped_unprepared(), 0);
}

fn instantiate(doc: &str, r: &MemoryResolver) -> Result<Plan, PlanError> {
    let graph = load_instrument(doc, &registry(), r).expect("loads").graph;
    Plan::instantiate(graph, AudioConfig::new(48_000.0, 64))
}

/// A fed input never compiles a value of its own: a kept pipe with a valid default feeds two
/// inputs whose preparer refuses `""`, and the instrument plans.
#[test]
fn wired_consumer_of_a_kept_pipe_compiles_empty_seed() {
    const DOC: &str = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str", "default": "abc" } } },
      "nodes": [ { "type": "nonempty", "address": "/a", "inputs": { "text": { "from": "/pat" } } },
                 { "type": "nonempty", "address": "/b", "inputs": { "text": { "from": "/pat" } } } ] }"#;
    let p = instantiate(DOC, &MemoryResolver::new());
    assert!(p.is_ok(), "{:?}", p.err());

    // An unfed input still compiles `""`, and is refused for it.
    const UNSET: &str = r#"{ "format_version": 3, "instrument": "t",
      "nodes": [ { "type": "nonempty", "address": "/a" } ] }"#;
    assert!(matches!(
        instantiate(UNSET, &MemoryResolver::new()),
        Err(PlanError::Prepare { .. })
    ));
}

/// A child's `str` interface input the parent feeds compiles nothing of its own either.
#[test]
fn fed_nested_pipe_compiles_empty_seed() {
    const CHILD: &str = r#"{ "format_version": 3, "instrument": "c",
      "interface": { "inputs": { "inner": { "type": "str" } } },
      "nodes": [ { "type": "nonempty", "address": "/w", "inputs": { "text": { "from": "/inner" } } } ] }"#;
    const DOC: &str = r#"{ "format_version": 3, "instrument": "t", "resources": { "c": "c.json" },
      "interface": { "inputs": { "nested": { "type": "str", "default": "abc" } } },
      "nodes": [ { "type": "subpatch", "address": "/sub", "patch": "c",
          "inputs": { "inner": { "from": "/nested" } } } ] }"#;
    let mut r = MemoryResolver::new();
    r.insert_text("c.json", CHILD);
    let p = instantiate(DOC, &r);
    assert!(p.is_ok(), "{:?}", p.err());
}

/// A child's interface input the parent feeds takes no send: routing drops it as a wired write, so
/// the routing table has no route there for a producer to prepare.
#[test]
fn routes_omit_a_fed_nested_pipe() {
    const CHILD: &str = r#"{ "format_version": 3, "instrument": "c",
      "interface": { "inputs": { "inner": { "type": "str" } } },
      "nodes": [ { "type": "words", "address": "/w", "inputs": { "text": { "from": "/inner" } } } ] }"#;
    const DOC: &str = r#"{ "format_version": 3, "instrument": "t", "resources": { "c": "c.json" },
      "interface": { "inputs": { "nested": { "type": "str" } } },
      "nodes": [ { "type": "subpatch", "address": "/sub", "patch": "c",
          "inputs": { "inner": { "from": "/nested" } } } ] }"#;
    let mut r = MemoryResolver::new();
    r.insert_text("c.json", CHILD);
    let p = instantiate(DOC, &r).expect("plans");
    let addresses: Vec<String> = p
        .prepared_routes()
        .iter()
        .map(|r| r.address.clone())
        .collect();
    assert_eq!(addresses, ["/nested/in"], "only the parent's own input");
    let mut e = Engine::new(p);
    block(&mut e);
    e.queue_osc("/sub/inner/in", &[words_arg("x")]);
    block(&mut e);
    assert_eq!(
        e.take_dropped_wired_writes(),
        1,
        "the send is a wired write"
    );
}

/// One preparer named by its crate-rooted path from two Operators has one kind, so a kept pipe
/// feeding both plans; reached through `super::` it has another, and the pipe is refused.
#[test]
fn a_preparer_named_by_its_crate_rooted_path_has_one_kind() {
    let doc = |second: &str| {
        format!(
            r#"{{ "format_version": 3, "instrument": "t",
      "interface": {{ "inputs": {{ "pat": {{ "type": "str", "default": "abc" }} }} }},
      "nodes": [ {{ "type": "nonempty", "address": "/a", "inputs": {{ "text": {{ "from": "/pat" }} }} }},
                 {{ "type": "{second}", "address": "/b", "inputs": {{ "text": {{ "from": "/pat" }} }} }} ] }}"#
        )
    };
    let p = instantiate(&doc("nonempty_too"), &MemoryResolver::new());
    assert!(p.is_ok(), "{:?}", p.err());
    assert!(matches!(
        instantiate(&doc("nonempty_super"), &MemoryResolver::new()),
        Err(PlanError::PreparerConflict { .. })
    ));
}

/// A top-level `str` interface input that feeds no preparing input loads with a warning saying a
/// send to it is dropped; one that does feed one loads clean.
#[test]
fn a_str_input_feeding_no_preparing_input_warns() {
    const ALONE: &str = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str", "default": "1" } } },
      "nodes": [ { "type": "osc_out", "address": "/o", "inputs": { "in": { "from": "/pat" } } } ] }"#;
    let loaded = load_instrument(ALONE, &registry(), &MemoryResolver::new()).expect("loads");
    assert!(
        matches!(loaded.warnings.as_slice(),
            [reuben_document::format::LoadWarning::UnreadStrInput { name }] if name == "pat"),
        "{:?}",
        loaded.warnings
    );
    let report = reuben_document::introspect::validate(ALONE, &registry(), &MemoryResolver::new());
    assert!(report.ok);
    assert!(report.warnings[0].message.contains("dropped"), "{report:?}");
    let loaded = load_instrument(DOC, &registry(), &MemoryResolver::new()).expect("loads");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
}

/// A dissolved interface input carried across a Swap: the install window and the first send after
/// it touch no heap on the render thread.
#[test]
fn dissolved_alias_carry_then_first_send_is_heap_free() {
    use reuben_core::coordinator::RenderSlot;
    use reuben_document::coordinator::Coordinator;
    const ONE: &str = r#"{ "format_version": 3, "instrument": "t",
      "interface": { "inputs": { "pat": { "type": "str", "default": "1" } } },
      "nodes": [ { "type": "steps", "address": "/a", "inputs": { "steps": { "from": "/pat" } } },
                 { "type": "steps", "address": "/z", "inputs": { "steps": "4" } } ] }"#;
    let (mut coord, side, _) = Coordinator::install_initial(
        ONE,
        registry(),
        Box::new(MemoryResolver::new()),
        AudioConfig::new(48_000.0, 128),
    )
    .expect("installs");
    let retired = coord.retired();
    let mut slot = RenderSlot::new(side);
    let mut out = vec![0.0f32; 128 * slot.channels()];
    for i in 0..4 {
        slot.queue_osc("/pat/in", &[steps_arg(&format!("{i}"))]);
        slot.fill(&mut out);
    }
    retired.drain();
    assert_counter_is_live();
    for round in 1..4 {
        let doc = ONE.replace(r#""steps": "4""#, &format!(r#""steps": "4 {round}""#));
        assert!(coord.swap_document(&doc).report.ok);
        let (mut allocs, mut frees) = (0, 0);
        for _ in 0..16 {
            let c = measure(|| slot.fill(&mut out));
            allocs += c.allocs;
            frees += c.frees;
        }
        assert_eq!((allocs, frees), (0, 0), "install window {round}");
        coord.try_reclaim();
        retired.drain();
        let send = [steps_arg("7 7")];
        let c = measure(|| {
            slot.queue_osc("/pat/in", &send);
            slot.fill(&mut out);
        });
        assert_eq!((c.allocs, c.frees), (0, 0), "round {round}");
        retired.drain();
    }
    assert_eq!(slot.take_dropped_unprepared(), 0);
}

/// A Swap refused for text that does not compile reports each failing value at its node and input,
/// as validation does.
#[test]
fn a_swap_refused_for_text_that_does_not_compile_attributes_every_failure() {
    use reuben_document::coordinator::Coordinator;
    let (mut coord, _side, _) = Coordinator::install_initial(
        DOC,
        registry(),
        Box::new(MemoryResolver::new()),
        AudioConfig::new(48_000.0, 64),
    )
    .expect("installs");
    let both = DOC
        .replace(r#""steps": "4 5""#, r#""steps": "4 x5""#)
        .replace(r#""default": "1 2 3""#, r#""default": "1 2 ?""#);
    let refused = coord.swap_document(&both);
    assert!(!refused.report.ok);
    let mut at: Vec<(Option<&str>, Option<&str>)> = refused
        .report
        .errors
        .iter()
        .map(|d| (d.node.as_deref(), d.port.as_deref()))
        .collect();
    at.sort();
    assert_eq!(
        at,
        [
            (Some("/literal"), Some("steps")),
            (Some("/pat"), Some("in"))
        ],
        "{:?}",
        refused.report
    );
    assert!(refused
        .report
        .errors
        .iter()
        .all(|d| d.message.contains("bytes")));
}

/// A `str` input wired into a subpatch whose child did not load warns about the missing child, not
/// that the input is unread.
#[test]
fn a_str_input_beside_a_missing_subpatch_warns_only_about_the_child() {
    use reuben_document::format::LoadWarning;
    const DOC: &str = r#"{ "format_version": 3, "instrument": "t", "resources": { "c": "missing.json" },
      "interface": { "inputs": { "pat": { "type": "str", "default": "1" } } },
      "nodes": [ { "type": "subpatch", "address": "/sub", "patch": "c",
          "inputs": { "inner": { "from": "/pat" } } } ] }"#;
    let loaded = load_instrument(DOC, &registry(), &MemoryResolver::new()).expect("loads dark");
    assert!(
        loaded
            .warnings
            .iter()
            .any(|w| matches!(w, LoadWarning::Nested { warning, .. }
                if matches!(**warning, LoadWarning::ResolveFailed { .. }))),
        "{:?}",
        loaded.warnings
    );
    assert!(
        !loaded
            .warnings
            .iter()
            .any(|w| matches!(w, LoadWarning::UnreadStrInput { .. })),
        "{:?}",
        loaded.warnings
    );
}
