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
use crate::descriptor::{Descriptor, LaneRule, Shape};
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
    /// unconnected. The inner length is the *source's* Lane count (1 broadcasts). A new-style
    /// materialized [`Shape::Float`](crate::descriptor::Shape) input that is **unwired** points
    /// at a dedicated single-Lane **scratch** buffer (see `materialize`) the engine fills each
    /// block, so the operator reads it through the same path as a wired source (ADR-0028).
    pub inputs: Vec<Option<Vec<usize>>>,
    /// Materialized Float inputs (ADR-0028): `(input port, scratch arena buffer)` for each
    /// **unwired** new-style Float input. The engine fills the buffer per block from
    /// `input_latches[port]`, writing mid-block changes at their frame. Wired Float inputs are
    /// absent — their dense source passes straight through.
    pub materialize: Vec<(usize, usize)>,
    /// Per `materialize` entry (same index): `true` once the scratch buffer holds the latch
    /// uniformly across the block, so a held-unchanged Float input can skip its refill (ADR-0028
    /// "cached, refilled only on change"). Carried across blocks. Render clears it the moment a
    /// mid-block change writes a gradient, and re-sets it after the next constant block refloods.
    /// Starts `false` (the freshly-allocated arena is zero, not the latch) so the first block fills.
    pub materialize_clean: Vec<bool>,
    /// Latched current scalar per input port (ADR-0028), carried across blocks. Meaningful only
    /// for ports listed in `materialize`; other slots are unused. Seeded from each input's default.
    pub input_latches: Vec<f32>,
    /// Per-input `varying` hint (ADR-0028), in input-port order — preallocated here (length known
    /// at instantiate) and reused every block so the audio thread never allocates it, even for an
    /// operator with >8 inputs. Initialised all-`true`; Render rewrites only the materialized
    /// ports each block (`false` ⇒ held unchanged this block). Legacy / wired ports stay `true`.
    pub varying: Vec<bool>,
    /// Held [`Shape::Enum`](crate::descriptor::Shape) value per input port (ADR-0028), as the
    /// variant index. In input-port order (length = input count); `0` for non-enum ports. Seeded
    /// from each enum input's default (or an author override) and carried across blocks; Render
    /// block-slices at enum changes and updates the slot at each change frame. Read via
    /// [`Io::enum_index`](crate::operator::Io::enum_index).
    pub enum_latches: Vec<usize>,
    /// For each output port: this node's per-Lane arena buffer indices (length `lanes`).
    pub outputs: Vec<Vec<usize>>,
    /// Message-edge routing (ADR-0014): for each of this node's Message output ports (in
    /// Message-output ordinal order — the index [`crate::operator::Io::emit`] uses), the
    /// node indices (into [`Plan::nodes`]) that receive its emissions. Empty for a node
    /// with no Message outputs.
    pub msg_targets: Vec<Vec<usize>>,
    /// Harmony-edge routing (ADR-0015): for each Harmony input port (Harmony-input ordinal
    /// order — the index [`crate::operator::Io::harmony`] uses), the source's context-arena
    /// slot, or `None` if unconnected (reads the default harmony). Followers read here.
    pub context_inputs: Vec<Option<usize>>,
    /// For each Harmony output port (Harmony-output ordinal order — the index
    /// [`crate::operator::Io::publish_harmony`] uses), this node's context-arena slot.
    pub context_outputs: Vec<usize>,
    /// For each Harmony output port, the node indices that read it — so a publish re-slices
    /// them (the third route lane). Sibling of `msg_targets`.
    pub ctx_targets: Vec<Vec<usize>>,
}

/// One master tap: a tapped port's per-Lane arena buffers, summed into the master output.
pub struct OutputTap {
    /// Logical master channel this tap feeds (ADR-0026), or `None` to broadcast to every
    /// channel (the historical mono fan).
    pub channel: Option<usize>,
    /// Per-Lane arena buffer indices of the tapped port; all summed.
    pub buffers: Vec<usize>,
}

/// The immutable execution image.
pub struct Plan {
    pub config: AudioConfig,
    /// Nodes in topological execution order.
    pub nodes: Vec<PlanNode>,
    /// Total number of edge buffers in the arena.
    pub num_buffers: usize,
    /// Length `num_buffers`: `true` at each arena slot that is a materialize **scratch** buffer
    /// (ADR-0028). Render skips these in its per-block "fresh edge buffers" clear, so a held Float
    /// input's buffer persists and need not be re-written every block (see `materialize_clean`).
    pub materialize_scratch_mask: Vec<bool>,
    /// Total number of context-arena slots (one per Harmony output port; ADR-0015).
    pub num_context_slots: usize,
    /// Master taps, summed into the per-channel master output (ADR-0026).
    pub output_taps: Vec<OutputTap>,
}

/// Why Instantiate failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// The graph has a cycle (feedback needs an explicit unit-delay; deferred).
    Cycle,
}

impl Plan {
    /// Instantiate a Graph into an executable Plan (the construction sub-step of a Swap).
    pub fn instantiate(mut graph: Graph, mut config: AudioConfig) -> Result<Plan, PlanError> {
        let order = topo_order(&graph)?;

        // Logical master width is derived from the instrument, not the device (ADR-0026):
        // the highest referenced channel index + 1, floored to stereo so a mono patch still
        // presents two channels. A broadcast tap (`None`) imposes no width on its own.
        config.channels = graph
            .outputs
            .iter()
            .filter_map(|(_, _, ch)| ch.map(|c| c + 1))
            .max()
            .unwrap_or(0)
            .max(AudioConfig::MIN_CHANNELS);

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
            .map(|(k, p, channel)| OutputTap {
                channel: *channel,
                buffers: out_buffers[*k][*p].clone(),
            })
            .collect();

        // Assign a context-arena slot per (node, Harmony output port). Independent of Lanes
        // (context is single-Lane, pre-fan-out; ADR-0015). `src_port` here is the full port
        // index, matching how connections reference ports.
        let mut next_ctx_slot = 0usize;
        let mut ctx_slot: SecondaryMap<NodeKey, std::collections::BTreeMap<usize, usize>> =
            SecondaryMap::new();
        for (key, node) in &graph.nodes {
            let mut m = std::collections::BTreeMap::new();
            for (port, p) in node.descriptor.outputs.iter().enumerate() {
                if p.shape == Shape::Harmony {
                    m.insert(port, next_ctx_slot);
                    next_ctx_slot += 1;
                }
            }
            ctx_slot.insert(key, m);
        }

        // Node index in execution order, for resolving Message-edge targets to Plan indices.
        let mut index_of: SecondaryMap<NodeKey, usize> = SecondaryMap::new();
        for (i, key) in order.iter().enumerate() {
            index_of.insert(*key, i);
        }

        // 3. Build PlanNodes in execution order, replicating operators per Lane.
        let mut nodes = Vec::with_capacity(order.len());
        // Arena slots that are materialize scratch (ADR-0028); Render skips them in its per-block
        // clear so held Float inputs persist. Collected as buffers are assigned below.
        let mut scratch_buffers: Vec<usize> = Vec::new();
        for key in &order {
            let n_lanes = lanes[*key];
            let descriptor = &graph.nodes[*key].descriptor;
            let overrides = &graph.nodes[*key].input_overrides;
            let enum_overrides = &graph.nodes[*key].enum_overrides;
            // Held enum value per input port (ADR-0028): an enum input's default (or an author
            // override), `0` elsewhere. Carried across blocks; Render updates it at change frames.
            let enum_latches: Vec<usize> = descriptor
                .inputs
                .iter()
                .enumerate()
                .map(|(port, p)| match &p.enum_meta {
                    Some(e) => enum_overrides
                        .iter()
                        .find(|(po, _)| *po == port)
                        .map(|(_, i)| *i)
                        .unwrap_or(e.default),
                    None => 0,
                })
                .collect();
            // Signal inputs wire to the source's arena buffers; Message inputs carry no
            // Signal data (events arrive via routing, ADR-0014) so they take no buffer. A
            // new-style materialized Float input that is unwired gets a dedicated scratch
            // buffer the engine fills from a latch (ADR-0028 materialize).
            let mut inputs: Vec<Option<Vec<usize>>> = Vec::with_capacity(descriptor.inputs.len());
            let mut materialize: Vec<(usize, usize)> = Vec::new();
            let mut input_latches: Vec<f32> = vec![0.0; descriptor.inputs.len()];
            // Preallocated once; Render reuses it every block (no audio-thread alloc, ADR-0028).
            let varying: Vec<bool> = vec![true; descriptor.inputs.len()];
            for (port, p) in descriptor.inputs.iter().enumerate() {
                // Only Float inputs take an arena buffer; Enum/Note/Harmony inputs carry no
                // Signal data (events/enum changes/context arrive via routing).
                if p.shape != Shape::Float {
                    inputs.push(None);
                    continue;
                }
                let wired = graph
                    .connections
                    .iter()
                    .find(|c| c.dst == *key && c.dst_port == port)
                    .map(|c| out_buffers[c.src][c.src_port].clone());
                match wired {
                    Some(bufs) => inputs.push(Some(bufs)),
                    None => match &p.meta {
                        // Materialized Float input, unwired: allocate a scratch buffer + seed the
                        // latch from the input's default. The operator reads it like any source.
                        Some(meta) => {
                            let buf = next_buffer;
                            next_buffer += 1;
                            // Seed the latch from an author override (a literal for this input),
                            // else the input's declared default (ADR-0028).
                            input_latches[port] = overrides
                                .iter()
                                .find(|(p, _)| *p == port)
                                .map(|(_, v)| *v)
                                .unwrap_or(meta.default);
                            materialize.push((port, buf));
                            scratch_buffers.push(buf);
                            inputs.push(Some(vec![buf]));
                        }
                        // Legacy signal input, unwired: the operator falls back to its
                        // same-named param (one-port-one-type), so it takes no buffer.
                        None => inputs.push(None),
                    },
                }
            }
            let outputs = out_buffers[*key].clone();
            // Message-edge targets, one entry per Message output port (ordinal order).
            let msg_targets = descriptor
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, p)| p.shape == Shape::Note)
                .map(|(port, _)| {
                    graph
                        .connections
                        .iter()
                        .filter(|c| c.src == *key && c.src_port == port)
                        .map(|c| index_of[c.dst])
                        .collect()
                })
                .collect();
            // Harmony-edge wiring (ADR-0015), Harmony-ordinal order.
            let context_inputs = descriptor
                .inputs
                .iter()
                .enumerate()
                .filter(|(_, p)| p.shape == Shape::Harmony)
                .map(|(port, _)| {
                    graph
                        .connections
                        .iter()
                        .find(|c| c.dst == *key && c.dst_port == port)
                        .map(|c| ctx_slot[c.src][&c.src_port])
                })
                .collect();
            let context_outputs = descriptor
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, p)| p.shape == Shape::Harmony)
                .map(|(port, _)| ctx_slot[*key][&port])
                .collect();
            let ctx_targets = descriptor
                .outputs
                .iter()
                .enumerate()
                .filter(|(_, p)| p.shape == Shape::Harmony)
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

            let materialize_clean = vec![false; materialize.len()];
            nodes.push(PlanNode {
                address: node.address,
                ops,
                descriptor: node.descriptor,
                params: node.params,
                lanes: n_lanes,
                inputs,
                materialize,
                materialize_clean,
                input_latches,
                varying,
                enum_latches,
                outputs,
                msg_targets,
                context_inputs,
                context_outputs,
                ctx_targets,
            });
        }

        let mut materialize_scratch_mask = vec![false; next_buffer];
        for b in scratch_buffers {
            materialize_scratch_mask[b] = true;
        }

        Ok(Plan {
            config,
            nodes,
            num_buffers: next_buffer,
            materialize_scratch_mask,
            num_context_slots: next_ctx_slot,
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
