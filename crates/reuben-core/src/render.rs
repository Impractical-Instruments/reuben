//! Render — executing a [`Plan`] per block (ADR-0009, ADR-0011).
//!
//! The serial executor walks the topologically-ordered nodes. For each node, incoming
//! Messages are routed: those whose local address names a param drive **block-slicing**
//! (the block is split at their frames so [`Operator::process`] always sees a constant
//! param value); everything else is delivered raw via [`Io::messages`] for event
//! operators (the Voicer) to time themselves. Output taps are summed into the master
//! buffer. Fan-in sums in fixed buffer order, so output is deterministic (ADR-0001).
//!
//! The [`Executor`] trait is the pluggable-executor seam (ADR-0001): it only decides the
//! order/concurrency of node processing. The MVP ships [`SerialExecutor`]; a parallel
//! cluster executor slots in behind the same trait later.

use crate::message::Message;
use crate::operator::Io;
use crate::plan::{Plan, PlanNode};

/// Decides the order in which nodes are processed for a block.
///
/// The plan is already topologically ordered, so a valid execution is simply
/// `0..nodes.len()`. A future parallel executor returns the same set grouped into
/// concurrently-runnable clusters.
pub trait Executor {
    fn order(&self, plan: &Plan) -> Vec<usize>;
}

/// Single-threaded executor: process nodes in topo order. (ADR-0001 MVP.)
#[derive(Default)]
pub struct SerialExecutor;

impl Executor for SerialExecutor {
    fn order(&self, plan: &Plan) -> Vec<usize> {
        (0..plan.nodes.len()).collect()
    }
}

/// Owns the edge-buffer arena and drives Render for a single Plan.
pub struct Renderer<E: Executor = SerialExecutor> {
    arena: Vec<Vec<f32>>,
    executor: E,
    block_size: usize,
}

impl Renderer<SerialExecutor> {
    /// Build a renderer for `plan` with the default serial executor.
    pub fn new(plan: &Plan) -> Self {
        Self::with_executor(plan, SerialExecutor)
    }
}

impl<E: Executor> Renderer<E> {
    pub fn with_executor(plan: &Plan, executor: E) -> Self {
        let block_size = plan.config.block_size;
        let arena = (0..plan.num_buffers).map(|_| vec![0.0; block_size]).collect();
        Self {
            arena,
            executor,
            block_size,
        }
    }

    /// Render one block. `messages` are the inputs for this block (frames in
    /// `0..block_size`); `out` is the master output buffer (length == block_size).
    pub fn render_block(&mut self, plan: &mut Plan, messages: &[Message], out: &mut [f32]) {
        debug_assert_eq!(out.len(), self.block_size);

        // Fresh edge buffers each block (upstream writes before downstream reads).
        for buf in &mut self.arena {
            buf.iter_mut().for_each(|s| *s = 0.0);
        }

        // Route messages to nodes, split into param updates vs raw events.
        let routes = route_messages(plan, messages);

        let sample_rate = plan.config.sample_rate;
        let block_size = self.block_size;
        for i in self.executor.order(plan) {
            let route = &routes[i];
            process_node(
                &mut self.arena,
                &mut plan.nodes[i],
                &route.params,
                &route.events,
                sample_rate,
                block_size,
            );
        }

        // Sum master taps.
        out.iter_mut().for_each(|s| *s = 0.0);
        for &tap in &plan.output_taps {
            for (o, s) in out.iter_mut().zip(&self.arena[tap]) {
                *o += *s;
            }
        }
    }
}

/// Per-node routed messages for one block.
#[derive(Default)]
struct NodeRoute {
    /// (frame, param slot, value) — drive block-slicing.
    params: Vec<(usize, usize, f32)>,
    /// Raw events (local address), absolute frames — for event operators.
    events: Vec<Message>,
}

/// Match each message to a node by address prefix, then classify param vs event.
fn route_messages(plan: &Plan, messages: &[Message]) -> Vec<NodeRoute> {
    let mut routes: Vec<NodeRoute> = (0..plan.nodes.len()).map(|_| NodeRoute::default()).collect();
    for msg in messages {
        for (i, node) in plan.nodes.iter().enumerate() {
            let Some(local) = local_address(&msg.addr, &node.address) else {
                continue;
            };
            match node.descriptor.param_index(local) {
                Some(slot) => {
                    if let Some(v) = msg.first_f32() {
                        let v = node.descriptor.params[slot].clamp(v);
                        routes[i].params.push((msg.frame, slot, v));
                    }
                }
                None => {
                    let mut ev = msg.clone();
                    ev.addr = local.to_string();
                    routes[i].events.push(ev);
                }
            }
            break; // a message targets at most one node
        }
    }
    for r in &mut routes {
        r.params.sort_by_key(|(f, _, _)| *f);
    }
    routes
}

/// Local address of `addr` relative to `node_addr`, if `addr` targets that node.
/// `/osc/freq` under `/osc` -> `freq`; `/osc` under `/osc` -> `` (whole-node).
fn local_address<'a>(addr: &'a str, node_addr: &str) -> Option<&'a str> {
    if addr == node_addr {
        return Some("");
    }
    let rest = addr.strip_prefix(node_addr)?;
    rest.strip_prefix('/')
}

/// Process one node for the block, applying block-slicing at param-message frames.
fn process_node(
    arena: &mut [Vec<f32>],
    node: &mut PlanNode,
    params: &[(usize, usize, f32)],
    events: &[Message],
    sample_rate: f32,
    block_size: usize,
) {
    // Segment boundaries: 0, every interior param frame, block_size.
    let mut bounds: Vec<usize> = Vec::with_capacity(params.len() + 2);
    bounds.push(0);
    for &(f, _, _) in params {
        if f > 0 && f < block_size {
            bounds.push(f);
        }
    }
    bounds.push(block_size);
    bounds.dedup();

    for w in bounds.windows(2) {
        let (seg_start, seg_end) = (w[0], w[1]);
        if seg_start >= seg_end {
            continue;
        }

        // Apply param updates landing exactly at this segment's start.
        for &(f, slot, v) in params {
            if f == seg_start {
                node.params[slot] = v;
            }
        }

        // Events whose frame falls in this segment, frames made segment-relative.
        let seg_events: Vec<Message> = events
            .iter()
            .filter(|m| m.frame >= seg_start && m.frame < seg_end)
            .map(|m| {
                let mut m = m.clone();
                m.frame -= seg_start;
                m
            })
            .collect();

        // Take this node's output buffers out of the arena to satisfy the borrow
        // checker (outputs are disjoint from inputs — no self-loops; cycles error).
        let mut outs: Vec<Vec<f32>> = node
            .outputs
            .iter()
            .map(|&i| std::mem::take(&mut arena[i]))
            .collect();

        // Scope the borrows of `arena` (inputs) and `outs` (outputs) so they release
        // before the output buffers are written back into the arena.
        {
            let in_refs: Vec<Option<&[f32]>> = node
                .inputs
                .iter()
                .map(|o| o.map(|i| &arena[i][seg_start..seg_end]))
                .collect();
            let mut out_slices: Vec<&mut [f32]> =
                outs.iter_mut().map(|b| &mut b[seg_start..seg_end]).collect();

            let mut io = Io::new(
                sample_rate,
                seg_end - seg_start,
                &in_refs,
                &mut out_slices,
                &node.params,
                &seg_events,
            );
            node.op.process(&mut io);
        }

        for (&i, buf) in node.outputs.iter().zip(outs) {
            arena[i] = buf;
        }
    }
}
