//! Plan — the static execution image produced by Instantiate (ADR-0009).
//!
//! Instantiate consumes a [`Graph`], topologically orders its nodes, and assigns each
//! output port a slot in the edge-buffer arena. The result is immutable and is what
//! [`crate::render`] executes per block. For the "first sound" run the schedule is a
//! plain linear topo order (the serial executor walks it in sequence); the parallel
//! cluster plan (ADR-0001) is a later refinement behind the same structure.

use slotmap::SecondaryMap;

use crate::config::AudioConfig;
use crate::descriptor::Descriptor;
use crate::graph::{Graph, NodeKey};
use crate::operator::Operator;

/// A node in execution order, with its arena buffer wiring resolved.
pub struct PlanNode {
    pub address: String,
    pub op: Box<dyn Operator>,
    pub descriptor: Descriptor,
    /// Current param values, in descriptor slot order. Mutated by Render (block-slicing).
    pub params: Vec<f32>,
    /// Arena buffer index feeding each input port, or `None` if unconnected.
    pub inputs: Vec<Option<usize>>,
    /// Arena buffer index each output port writes to.
    pub outputs: Vec<usize>,
}

/// The immutable execution image.
pub struct Plan {
    pub config: AudioConfig,
    /// Nodes in topological execution order.
    pub nodes: Vec<PlanNode>,
    /// Total number of edge buffers in the arena.
    pub num_buffers: usize,
    /// Arena indices summed into the rendered master output.
    pub output_taps: Vec<usize>,
}

/// Why Instantiate failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The graph has a cycle (feedback needs an explicit unit-delay; deferred).
    Cycle,
}

impl Plan {
    /// Instantiate a Graph into an executable Plan (the construction sub-step of a Swap).
    pub fn instantiate(mut graph: Graph, config: AudioConfig) -> Result<Plan, PlanError> {
        // Assign every (node, output port) a unique arena buffer index.
        let mut next_buffer = 0usize;
        let mut out_buffers: SecondaryMap<NodeKey, Vec<usize>> = SecondaryMap::new();
        for (key, node) in &graph.nodes {
            let bufs = (0..node.descriptor.outputs.len())
                .map(|_| {
                    let i = next_buffer;
                    next_buffer += 1;
                    i
                })
                .collect();
            out_buffers.insert(key, bufs);
        }

        let order = topo_order(&graph)?;

        let output_taps = graph
            .outputs
            .iter()
            .map(|(k, p)| out_buffers[*k][*p])
            .collect();

        let mut nodes = Vec::with_capacity(order.len());
        for key in &order {
            // Resolve inputs against connections before removing the node.
            let in_count = graph.nodes[*key].descriptor.inputs.len();
            let inputs = (0..in_count)
                .map(|port| {
                    graph
                        .connections
                        .iter()
                        .find(|c| c.dst == *key && c.dst_port == port)
                        .map(|c| out_buffers[c.src][c.src_port])
                })
                .collect();
            let outputs = out_buffers[*key].clone();
            let node = graph.nodes.remove(*key).expect("key from topo order");
            nodes.push(PlanNode {
                address: node.address,
                op: node.op,
                descriptor: node.descriptor,
                params: node.params,
                inputs,
                outputs,
            });
        }

        Ok(Plan {
            config,
            nodes,
            num_buffers: next_buffer,
            output_taps,
        })
    }
}

/// Kahn topological sort; deterministic given graph key order. Errors on cycle.
fn topo_order(graph: &Graph) -> Result<Vec<NodeKey>, PlanError> {
    let mut indegree: SecondaryMap<NodeKey, usize> =
        graph.nodes.keys().map(|k| (k, 0usize)).collect();
    for c in &graph.connections {
        indegree[c.dst] += 1;
    }

    // Seed with zero-indegree nodes in stable key order.
    let mut queue: Vec<NodeKey> = graph
        .nodes
        .keys()
        .filter(|k| indegree[*k] == 0)
        .collect();
    let mut order = Vec::with_capacity(graph.nodes.len());

    while let Some(k) = queue.pop() {
        order.push(k);
        for c in graph.connections.iter().filter(|c| c.src == k) {
            let d = &mut indegree[c.dst];
            *d -= 1;
            if *d == 0 {
                queue.push(c.dst);
            }
        }
    }

    if order.len() != graph.nodes.len() {
        return Err(PlanError::Cycle);
    }
    Ok(order)
}
