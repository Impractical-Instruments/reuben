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

use reuben_core::descriptor::{Descriptor, LaneRule, ParamMeta, Port, PortType};
use reuben_core::graph::Graph;
use reuben_core::operator::{Io, Operator};
use reuben_core::plan::{port_kind, Plan, PlanError, PortKind};
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
        lanes: LaneRule::Inherit,
    }
}

/// A Value (`f32`) control port — declared with meta, the materialized-scalar form.
fn value_port(name: &'static str) -> Port {
    Port::float(ParamMeta {
        name,
        min: -1_000_000.0,
        max: 1_000_000.0,
        default: 0.0,
        unit: "",
        curve: reuben_core::descriptor::Curve::Linear,
    })
}

/// A bare scalar **output** port (`f32`, no meta) — a Value source like `voicer.freq` / `clock.gate`.
fn value_out(name: &'static str) -> Port {
    Port {
        name,
        ty: PortType::F32,
        meta: None,
    }
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
    let plan = wire(Port::buffer("o"), Port::buffer("i")).expect("a valid wire instantiates");
    assert_eq!(plan.nodes.len(), 2);
}

#[test]
fn port_form_reads_a_declared_input_form() {
    let plan = wire(Port::buffer("o"), Port::buffer("i")).expect("instantiate");
    // The sink node (index varies with topo order); find it by address.
    let dst = plan.nodes.iter().position(|n| n.address == "/dst").unwrap();
    assert_eq!(port_form(&plan, dst, 0), port_kind(&Port::buffer("i")));
}

#[test]
fn signal_buffer_count_counts_the_signal_edge() {
    // One Signal source feeding a Signal sink: a single shared edge buffer.
    let plan = wire(Port::buffer("o"), Port::buffer("i")).expect("instantiate");
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
        desc("a", vec![value_port("i")], vec![value_out("o")]),
    );
    let b = g.add_boxed(
        "/b",
        Box::new(Probe),
        desc("b", vec![value_port("i")], vec![value_out("o")]),
    );
    g.connect(a, 0, b, 0);
    g.connect(b, 0, a, 0);
    match Plan::instantiate(g, AudioConfig::new(48_000.0, 128)) {
        Err(e) => assert_eq!(e, PlanError::Cycle),
        Ok(_) => panic!("a cycle must not instantiate"),
    }
}
