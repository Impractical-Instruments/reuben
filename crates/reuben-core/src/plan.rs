//! Plan — the static execution image produced by Instantiate (ADR-0009, ADR-0010).
//!
//! Instantiate consumes a [`Graph`], topologically orders its nodes, computes each node's
//! **Lane (Voice) count**, replicates Lane-expanded operators per Voice with independent
//! state, and assigns every (output port, Lane) a slot in the edge-buffer arena. The
//! result is immutable and is what [`crate::render`] executes per block.
//!
//! Lane counts: a node whose descriptor declares [`LaneRule::FromParam`] (the Voicer)
//! expands to that param's value; every other node inherits the max Lane count of its
//! inputs (1 if it has none). Because all fan-out shapes are fixed here, Render pays
//! nothing for them (ADR-0010).

use slotmap::SecondaryMap;

use crate::config::AudioConfig;
use crate::descriptor::{Descriptor, LaneRule, PortKind};
use crate::graph::{Graph, NodeKey};
use crate::operator::Operator;

/// A node in execution order, with its Lane fan-out and arena buffer wiring resolved.
pub struct PlanNode {
    pub address: String,
    /// One operator instance per Lane (Voice); `ops.len() == lanes`. Lane 0 is the
    /// graph's original instance; the rest are fresh-state [`Operator::spawn`] copies.
    pub ops: Vec<Box<dyn Operator>>,
    pub descriptor: Descriptor,
    /// Current param values, in descriptor slot order. Shared across Lanes; mutated by
    /// Render (block-slicing).
    pub params: Vec<f32>,
    /// Lane (Voice) count at this node.
    pub lanes: usize,
    /// For each input port: the source's per-Lane arena buffer indices, or `None` if
    /// unconnected. The inner length is the *source's* Lane count (1 broadcasts).
    pub inputs: Vec<Option<Vec<usize>>>,
    /// For each output port: this node's per-Lane arena buffer indices (length `lanes`).
    pub outputs: Vec<Vec<usize>>,
    /// Message-edge routing (ADR-0014): for each of this node's Message output ports (in
    /// Message-output ordinal order — the index [`crate::operator::Io::emit`] uses), the
    /// node indices (into [`Plan::nodes`]) that receive its emissions. Empty for a node
    /// with no Message outputs.
    pub msg_targets: Vec<Vec<usize>>,
}

/// The immutable execution image.
pub struct Plan {
    pub config: AudioConfig,
    /// Nodes in topological execution order.
    pub nodes: Vec<PlanNode>,
    /// Total number of edge buffers in the arena.
    pub num_buffers: usize,
    /// Master taps: each is a tapped port's per-Lane buffers, all summed into the output.
    pub output_taps: Vec<Vec<usize>>,
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
        let order = topo_order(&graph)?;

        // 1. Lane count per node, in topo order (sources resolved before dependents).
        let mut lanes: SecondaryMap<NodeKey, usize> = SecondaryMap::new();
        for key in &order {
            let node = &graph.nodes[*key];
            let count = match node.descriptor.lanes {
                LaneRule::FromParam(slot) => {
                    (node.params.get(slot).copied().unwrap_or(1.0).round() as usize).max(1)
                }
                LaneRule::Inherit => graph
                    .connections
                    .iter()
                    .filter(|c| c.dst == *key)
                    .map(|c| lanes.get(c.src).copied().unwrap_or(1))
                    .max()
                    .unwrap_or(1),
            };
            lanes.insert(*key, count);
        }

        // 2. Assign every (node, output port, Lane) a unique arena buffer index.
        let mut next_buffer = 0usize;
        let mut out_buffers: SecondaryMap<NodeKey, Vec<Vec<usize>>> = SecondaryMap::new();
        for (key, node) in &graph.nodes {
            let n_lanes = lanes[key];
            let ports = (0..node.descriptor.outputs.len())
                .map(|_| {
                    (0..n_lanes)
                        .map(|_| {
                            let i = next_buffer;
                            next_buffer += 1;
                            i
                        })
                        .collect()
                })
                .collect();
            out_buffers.insert(key, ports);
        }

        let output_taps = graph
            .outputs
            .iter()
            .map(|(k, p)| out_buffers[*k][*p].clone())
            .collect();

        // Node index in execution order, for resolving Message-edge targets to Plan indices.
        let mut index_of: SecondaryMap<NodeKey, usize> = SecondaryMap::new();
        for (i, key) in order.iter().enumerate() {
            index_of.insert(*key, i);
        }

        // 3. Build PlanNodes in execution order, replicating operators per Lane.
        let mut nodes = Vec::with_capacity(order.len());
        for key in &order {
            let n_lanes = lanes[*key];
            let descriptor = &graph.nodes[*key].descriptor;
            // Signal inputs wire to the source's arena buffers; Message inputs carry no
            // Signal data (events arrive via routing, ADR-0014) so they take no buffer.
            let inputs = descriptor
                .inputs
                .iter()
                .enumerate()
                .map(|(port, p)| {
                    if p.kind == PortKind::Message {
                        return None;
                    }
                    graph
                        .connections
                        .iter()
                        .find(|c| c.dst == *key && c.dst_port == port)
                        .map(|c| out_buffers[c.src][c.src_port].clone())
                })
                .collect();
            let outputs = out_buffers[*key].clone();
            // Message-edge targets, one entry per Message output port (ordinal order).
            let msg_targets = descriptor
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, p)| p.kind == PortKind::Message)
                .map(|(port, _)| {
                    graph
                        .connections
                        .iter()
                        .filter(|c| c.src == *key && c.src_port == port)
                        .map(|c| index_of[c.dst])
                        .collect()
                })
                .collect();

            let node = graph.nodes.remove(*key).expect("key from topo order");
            let mut ops: Vec<Box<dyn Operator>> = Vec::with_capacity(n_lanes);
            ops.push(node.op);
            for _ in 1..n_lanes {
                ops.push(ops[0].spawn());
            }

            nodes.push(PlanNode {
                address: node.address,
                ops,
                descriptor: node.descriptor,
                params: node.params,
                lanes: n_lanes,
                inputs,
                outputs,
                msg_targets,
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
    let mut queue: Vec<NodeKey> = graph.nodes.keys().filter(|k| indegree[*k] == 0).collect();
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
