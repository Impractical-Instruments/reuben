//! ADR-0031 wire-form oracle + per-wire checker fixtures.
//!
//! Built test-first as the spine's substrate (impl-prep §1). A port's **form** is *declared* by its
//! [`PortType`] — `f32` = Value, `f32_buffer` = Signal, a struct vocab (`Note`) = Event — and the
//! planner's only form job is a **local per-wire check**: Value→Signal materializes, Signal→Value is
//! a hard error, like→like is direct (ADR-0031). These fixtures pin that check.
//!
//! The fixtures wire **synthetic single-port operators** (one declared form each) so a plan's
//! buffer count isolates the wire under test: [`signal_buffer_count`] == declared-Signal ports +
//! materialized Value→Signal edges. Real operators carry their forms after the step-4 sweep; until
//! then these probes are the oracle.

use reuben_core::descriptor::{Descriptor, Port, PortType};
use reuben_core::graph::Graph;
use reuben_core::operator::{Io, Operator};
use reuben_core::plan::{port_kind, Plan, PlanError, PortKind};
use reuben_core::vocab::FilterMode;
use reuben_core::AudioConfig;

// ----------------------------------------------------------------------------------------------
// Synthetic single-port operators. `add_boxed` takes the descriptor explicitly, so one no-op
// `Probe` body backs every shape — the descriptor is what carries the declared form under test.
// ----------------------------------------------------------------------------------------------

struct Probe;

impl Operator for Probe {
    fn descriptor() -> Descriptor {
        // Never called: every Probe is added via `add_boxed` with an explicit descriptor.
        desc("probe", vec![], vec![])
    }
    fn process(&mut self, _io: &mut Io) {}
    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Probe)
    }
}

fn desc(type_name: &'static str, inputs: Vec<Port>, outputs: Vec<Port>) -> Descriptor {
    Descriptor {
        type_name,
        inputs,
        outputs,
        params: vec![],
        resources: vec![],
        constant_param: None,
    }
}

/// A Signal port — a dense per-sample buffer (`f32_buffer` audio: an LFO out, `filter.cutoff`).
fn signal(name: &'static str) -> Port {
    Port::f32_buffer(name)
}

/// A Value port — a latched single value. Modelled with `I32` so it classifies Value *now*; until
/// the step-4 sweep `F32` still classifies Signal (decision A), so a genuine numeric Value source
/// is `I32` here. The real `f32`-Value fixtures (C/E/F: `tempo`, gate spine) arrive at step 4.
fn value(name: &'static str) -> Port {
    Port {
        name,
        ty: PortType::I32,
        meta: None,
    }
}

/// A Value port carrying an enum (`filter.mode`) — a Value-only type with no buffer form.
fn value_enum(name: &'static str) -> Port {
    Port::enumerated(FilterMode::enum_meta(name))
}

/// An Event port — a sparse frame-stamped stream (`Note`: a sequencer's `degrees` out).
fn event(name: &'static str) -> Port {
    Port::note(name)
}

// ----------------------------------------------------------------------------------------------
// Oracle probes (impl-prep §1).
// ----------------------------------------------------------------------------------------------

/// The declared form of an input port, read from the plan's precomputed classification.
fn port_form(plan: &Plan, node: usize, port: usize) -> PortKind {
    plan.nodes[node].input_kinds[port]
}

/// Buffer cost of a plan: declared-Signal ports + materialized Value→Signal edges. With
/// single-port synthetic operators this isolates the wire under test.
fn signal_buffer_count(plan: &Plan) -> usize {
    plan.num_buffers
}

/// Wire one source-output form to one sink-input form through the real planner.
fn wire(src: Port, dst: Port) -> Result<Plan, PlanError> {
    let mut g = Graph::new();
    let s = g.add_boxed("/src", Box::new(Probe), desc("src", vec![], vec![src]));
    let d = g.add_boxed("/dst", Box::new(Probe), desc("dst", vec![dst], vec![]));
    g.connect(s, 0, d, 0);
    Plan::instantiate(g, AudioConfig::new(48_000.0, 128))
}

// ----------------------------------------------------------------------------------------------
// Step 0 — oracle substrate (tracer bullets).
// ----------------------------------------------------------------------------------------------

#[test]
fn graph_helper_wires_two_nodes_and_instantiates() {
    // Tracer bullet: the thin Graph helper builds a real Plan from two wired nodes.
    let plan =
        wire(Port::f32_buffer("o"), Port::f32_buffer("i")).expect("a valid wire instantiates");
    assert_eq!(plan.nodes.len(), 2);
}

#[test]
fn port_form_reads_a_declared_input_form() {
    let plan = wire(Port::f32_buffer("o"), Port::f32_buffer("i")).expect("instantiate");
    // The sink node (index varies with topo order); find it by address.
    let dst = plan.nodes.iter().position(|n| n.address == "/dst").unwrap();
    assert_eq!(port_form(&plan, dst, 0), port_kind(&Port::f32_buffer("i")));
}

#[test]
fn signal_buffer_count_counts_the_signal_edge() {
    // One Signal source feeding a Signal sink: a single shared edge buffer.
    let plan = wire(Port::f32_buffer("o"), Port::f32_buffer("i")).expect("instantiate");
    assert_eq!(signal_buffer_count(&plan), 1);
}

#[test]
fn helper_surfaces_plan_errors_as_err_not_panic() {
    // A two-node cycle is the error the planner already rejects; the helper returns it as `Err`
    // (the coercion fixtures G/H/I will assert `Err(FormMismatch)` over the same surface).
    let mut g = Graph::new();
    let a = g.add_boxed(
        "/a",
        Box::new(Probe),
        desc("a", vec![value("i")], vec![value("o")]),
    );
    let b = g.add_boxed(
        "/b",
        Box::new(Probe),
        desc("b", vec![value("i")], vec![value("o")]),
    );
    g.connect(a, 0, b, 0);
    g.connect(b, 0, a, 0);
    match Plan::instantiate(g, AudioConfig::new(48_000.0, 128)) {
        Err(e) => assert_eq!(e, PlanError::Cycle),
        Ok(_) => panic!("a cycle must not instantiate"),
    }
}

// ----------------------------------------------------------------------------------------------
// Step 1 — per-wire form checker (impl-prep §2). Synthetic ports isolate each form crossing; the
// real-port versions (C/E/F numeric Value spine) light up at step 4 as operators migrate.
// ----------------------------------------------------------------------------------------------

fn dst_idx(plan: &Plan) -> usize {
    plan.nodes.iter().position(|n| n.address == "/dst").unwrap()
}

/// A — Value→Signal is the one implicit coercion: the Value source materializes a (constant)
/// buffer at the Signal input. One buffer, the materialized edge.
#[test]
fn value_into_signal_input_materializes_one_buffer() {
    let plan = wire(value("o"), signal("i")).expect("Value→Signal is legal");
    let dst = dst_idx(&plan);
    assert_eq!(port_form(&plan, dst, 0), PortKind::Signal);
    assert!(
        !plan.nodes[dst].materialize.is_empty(),
        "the Signal input is fed by a Value source, so it materializes"
    );
    assert_eq!(signal_buffer_count(&plan), 1);
}

/// B — Signal→Signal is a plain wire: the sink shares the source's edge buffer, no coercion.
#[test]
fn signal_into_signal_input_is_a_direct_shared_edge() {
    let plan = wire(signal("o"), signal("i")).expect("Signal→Signal is legal");
    let dst = dst_idx(&plan);
    assert_eq!(port_form(&plan, dst, 0), PortKind::Signal);
    assert!(
        plan.nodes[dst].materialize.is_empty(),
        "a Signal source shares its buffer; nothing materializes"
    );
    assert_eq!(signal_buffer_count(&plan), 1);
}

/// C — Value→Value is direct and costs no buffer: a held knob never materializes.
#[test]
fn value_into_value_input_is_direct_and_bufferless() {
    let plan = wire(value("o"), value("i")).expect("Value→Value is legal");
    let dst = dst_idx(&plan);
    assert_eq!(port_form(&plan, dst, 0), PortKind::Value);
    assert_eq!(signal_buffer_count(&plan), 0);
}

/// G — Signal→Value is the headline hard error: there is no implicit sample-and-hold, and the
/// message must name the missing converter (a user *will* try this wire). Deliberate gap.
#[test]
fn signal_into_value_input_is_a_hard_error_naming_the_converter() {
    match wire(signal("o"), value("i")).err() {
        Some(PlanError::FormMismatch { src, dst, reason }) => {
            assert_eq!(src, "/src.o");
            assert_eq!(dst, "/dst.i");
            assert!(
                reason.contains("envelope follower") || reason.contains("quantizer"),
                "Signal→Value error must name the converter op: {reason}"
            );
        }
        other => panic!("expected FormMismatch, got {other:?}"),
    }
}

/// H — Signal into a Value-only type (an enum) is equally illegal, and the message must explain the
/// enum case (a discrete choice, not a per-sample signal) — *not* dangle the numeric-Value converter
/// hint, since no envelope-follower/quantizer produces an enum.
#[test]
fn signal_into_enum_value_input_is_a_hard_error_explaining_the_enum() {
    match wire(signal("o"), value_enum("mode")).err() {
        Some(PlanError::FormMismatch { dst, reason, .. }) => {
            assert_eq!(dst, "/dst.mode");
            assert!(
                reason.contains("enum") && reason.contains("discrete choice"),
                "Signal→enum error must explain the enum target: {reason}"
            );
            assert!(
                !reason.contains("envelope follower"),
                "must not dangle the numeric converter hint for an enum: {reason}"
            );
        }
        other => panic!("expected FormMismatch, got {other:?}"),
    }
}

/// I — Event→Signal is illegal: a note stream cannot feed a per-sample input without an explicit op,
/// and the message must name the missing latch / change-detect.
#[test]
fn event_into_signal_input_is_a_hard_error_naming_the_latch() {
    match wire(event("o"), signal("i")).err() {
        Some(PlanError::FormMismatch { reason, .. }) => assert!(
            reason.contains("latch") || reason.contains("change-detect"),
            "Event→Signal error must name the latch / change-detect op: {reason}"
        ),
        other => panic!("expected FormMismatch, got {other:?}"),
    }
}
