//! Interface-pipe dissolution: a chain of pipes collapses and its aliases land on the real port.
//!
//! The subject is the loader's dissolve pass, so the test lives with the loader; what it asserts on
//! is the Plan that comes out, read through `Plan::nodes` / `Plan::input_aliases`.

use reuben_core::resources::SampleBuffer;
use reuben_core::{AudioConfig, Plan, Registry};
use reuben_document::resources::{ResolveError, ResourceResolver};

/// A gain cell behind an `in` boundary.
const INNER: &str = r#"{"format_version":2,"instrument":"inner",
    "interface":{"inputs":{"in":{"type":"f32_buffer"}},
                 "outputs":{"out":{"from":"/g.out"}}},
    "nodes":[{"type":"mul_f32_signal","address":"/g",
              "inputs":{"a":{"from":"/in"},"b":0.5}}]}"#;

/// `inner` re-exported through a second boundary — so a top-level wire reaches the gain
/// through *two* pipes, one per nesting level.
const OUTER: &str = r#"{"format_version":2,"instrument":"outer",
    "resources":{"inner":"inner.json"},
    "interface":{"inputs":{"in":{"type":"f32_buffer"}},
                 "outputs":{"out":{"from":"/s.out"}}},
    "nodes":[{"type":"subpatch","address":"/s","patch":"inner",
              "inputs":{"in":{"from":"/in"}}}]}"#;

const TOP: &str = r#"{"format_version":2,"instrument":"top",
    "resources":{"outer":"outer.json"},
    "interface":{"outputs":{"out":{"from":"/out.audio"}}},
    "nodes":[{"type":"oscillator","address":"/o"},
             {"type":"subpatch","address":"/w","patch":"outer",
              "inputs":{"in":{"from":"/o"}}},
             {"type":"output","address":"/out","inputs":{"audio":{"from":"/w.out"}}}]}"#;

struct Nested;
impl ResourceResolver for Nested {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        match source {
            "inner.json" => Ok(INNER.to_string()),
            "outer.json" => Ok(OUTER.to_string()),
            _ => Err(ResolveError::NotFound(source.to_string())),
        }
    }
}

/// Two nested boundaries put two pipes in a row between the oscillator and the gain. Both
/// collapse, and the alias of the *outer* one — whose consumer was the inner pipe, itself
/// since dissolved — has to follow through to the gain rather than dangle at a node that is
/// no longer in the schedule.
///
/// The follow-through is bookkeeping kept on the side of the fixpoint rather than re-derived
/// from it, so it is worth pinning behaviorally: an alias that stops following its hops still
/// names a plausible node index and goes on routing messages, to the wrong port.
#[test]
fn a_chain_of_two_pipes_collapses_and_both_aliases_land_on_the_gain() {
    let loaded =
        reuben_document::load_instrument(TOP, &Registry::builtin(), &Nested).expect("loads");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let plan =
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, 64)).expect("instantiates");

    let gain = plan
        .nodes()
        .iter()
        .position(|n| &*n.address == "/w/s/g")
        .expect("the inner gain survives the splice");
    let a = plan.nodes()[gain]
        .descriptor
        .inputs
        .iter()
        .position(|p| p.name == "a")
        .expect("`a` input");

    // No pipe is left in the schedule: both levels' boundaries dissolved.
    assert!(
        !plan
            .nodes()
            .iter()
            .any(|n| n.descriptor.type_name == "pipe"),
        "a pipe stayed in the schedule: {:?}",
        plan.nodes()
            .iter()
            .filter(|n| n.descriptor.type_name == "pipe")
            .map(|n| &n.address)
            .collect::<Vec<_>>()
    );

    // Both minted addresses stay addressable, and both name the gain's `a` port.
    let mut addrs: Vec<&str> = plan
        .input_aliases()
        .iter()
        .map(|al| {
            assert_eq!((al.node, al.dst_port), (gain, a), "{} dangles", al.address);
            &*al.address
        })
        .collect();
    addrs.sort_unstable();
    assert_eq!(addrs, ["/w/in", "/w/s/in"]);

    // The oscillator's buffer reaches the gain zero-copy — the collapse rewired the feeder
    // wire through, rather than leaving the gain on a materialized scratch.
    let osc = plan
        .nodes()
        .iter()
        .position(|n| &*n.address == "/o")
        .expect("/o node");
    assert_eq!(
        plan.nodes()[gain].inputs[a],
        Some(plan.nodes()[osc].outputs[0]),
        "the gain does not share the oscillator's buffer"
    );
}
