//! Render — executing a [`Plan`] per block (ADR-0009, ADR-0010, ADR-0011, ADR-0030).
//!
//! The serial executor walks the topologically-ordered nodes. For each node, incoming
//! Messages are routed to its input ports by the port's [`PortKind`](crate::plan::PortKind):
//! a **Held** control (scalar / enum / `Harmony`) drives **block-slicing** (the block is split
//! at its change frames so [`Operator::process`] always sees a constant held value, read via
//! `io.last`); a **Stream** event (`Note`) is delivered as a zero-copy [`Event`] on its port
//! (read via `io.stream`); a **Dense** [`Buffer`] input fed by a scalar is **materialized** ZOH
//! into its arena buffer (read via `io.signal`).
//!
//! Each node is processed once **per Lane (Voice)**: the engine runs `node.ops[lane]`
//! with that Lane's input/output buffers and `io.lane()` set, so single-Lane operators
//! are transparently replicated (ADR-0010). A single-Lane source feeding a multi-Lane
//! node broadcasts. Master taps sum every Lane of the tapped port, in fixed order, so
//! output is deterministic (ADR-0001).
//!
//! **Realtime-safe.** A [`Renderer`] preallocates its edge-buffer arena and every piece
//! of per-block scratch at construction, and reuses them; steady-state [`Renderer::render_block`]
//! performs no heap allocation (verified by `tests/rt_safe.rs`). Per-Lane buffer wiring
//! uses stack [`SmallVec`]s; routed events are zero-copy views onto the caller's Messages.
//!
//! The [`Executor`] trait is the pluggable-executor seam (ADR-0001).

use smallvec::SmallVec;

use crate::descriptor::{Port, PortType};
use crate::message::{Arg, Emit, Event, Message};
use crate::operator::Io;
use crate::plan::{port_kind, Plan, PlanNode, PortKind};

/// Decides the order in which nodes are processed for a block.
///
/// The plan is already topologically ordered, so a valid execution is simply
/// `0..nodes.len()`. A future parallel executor returns the same set grouped into
/// concurrently-runnable clusters. The order is written into a caller-owned buffer
/// (reused across blocks) so producing it allocates nothing in steady state.
pub trait Executor {
    fn order(&self, plan: &Plan, out: &mut Vec<usize>);
}

/// Single-threaded executor: process nodes in topo order. (ADR-0001 MVP.)
#[derive(Default)]
pub struct SerialExecutor;

impl Executor for SerialExecutor {
    fn order(&self, plan: &Plan, out: &mut Vec<usize>) {
        out.clear();
        out.extend(0..plan.nodes.len());
    }
}

/// Preallocated capacity for the per-block emit pool and scratch. Sized to absorb a
/// typical block's emissions without reallocating on the audio thread; emitting beyond it
/// grows the Vec once (allocation), which steady-state graphs do not reach.
const EMIT_POOL_CAP: usize = 256;

/// Owns the edge-buffer arena and drives Render for a single Plan.
///
/// All buffers and per-block scratch are allocated once here and reused, so
/// [`Renderer::render_block`] is allocation-free in steady state.
pub struct Renderer<E: Executor = SerialExecutor> {
    /// Edge buffers, indexed by arena slot; one block long each.
    arena: Vec<Vec<f32>>,
    /// Per-node scratch: the current node's output buffers, swapped out of `arena` so
    /// inputs (still in `arena`) and outputs are disjointly borrowable. Flat layout,
    /// index `port * lanes + lane`. Cleared and refilled per node — capacity retained.
    out_scratch: Vec<Vec<f32>>,
    /// Per-block message routing, one entry per node. Reused; inner Vecs cleared per block.
    routes: Vec<NodeRoute>,
    /// Block-lifetime pool of operator-emitted Messages (ADR-0014). Routed events borrow
    /// from it, so it is grown once and only cleared (never freed) per block.
    emitted: Vec<Emit>,
    /// One node's emissions for the current node, drained into `emitted` after it runs.
    emit_scratch: Vec<Emit>,
    /// Throwaway outbound sink for the mono [`Renderer::render_block`] convenience, which has no
    /// outbound out-parameter. Preallocated and cleared per call so render_block stays alloc-free.
    outbound_sink: Vec<Message>,
    /// Per-node block-slice boundaries scratch. Cleared per node — capacity retained.
    bounds: Vec<usize>,
    /// Execution order for the block, refilled by the executor into reused capacity.
    order: Vec<usize>,
    /// Per-channel master scratch (ADR-0026), one buffer per logical channel, each
    /// `block_size` long. Used by the mono [`Renderer::render_block`] convenience so it can
    /// compute the full N-channel master and hand back channel 0; preallocated and reused.
    master: Vec<Vec<f32>>,
    /// The pluggable executor (ADR-0001). Decides node order each block.
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
        let arena = (0..plan.num_buffers)
            .map(|_| vec![0.0; block_size])
            .collect();

        // Preallocate scratch sized to the plan so the steady-state render path never grows it.
        let routes = (0..plan.nodes.len())
            .map(|_| NodeRoute::default())
            .collect();
        let max_out_bufs = plan
            .nodes
            .iter()
            .map(|n| n.outputs.iter().map(|p| p.len()).sum::<usize>())
            .max()
            .unwrap_or(0);
        let out_scratch = Vec::with_capacity(max_out_bufs);
        let bounds = Vec::with_capacity(8);
        let order = Vec::with_capacity(plan.nodes.len());

        Self {
            arena,
            out_scratch,
            routes,
            emitted: Vec::with_capacity(EMIT_POOL_CAP),
            emit_scratch: Vec::with_capacity(EMIT_POOL_CAP),
            outbound_sink: Vec::with_capacity(EMIT_POOL_CAP),
            bounds,
            order,
            master: (0..plan.config.channels)
                .map(|_| vec![0.0; block_size])
                .collect(),
            executor,
            block_size,
        }
    }

    /// Render one block into a mono buffer — the historical convenience, kept for tests,
    /// examples, and single-channel callers. `out` is `block_size` long and receives **logical
    /// channel 0** of the master. For a broadcast/mono instrument every channel is identical,
    /// so this is bit-identical to the pre-stereo output; for a true stereo patch it is the
    /// left channel only (use [`Renderer::render_block_multi`] for both). Allocation-free.
    pub fn render_block(&mut self, plan: &mut Plan, messages: &[Message], out: &mut [f32]) {
        debug_assert_eq!(out.len(), self.block_size);
        // Borrow `master`/`outbound_sink` out of `self` so `render_into` can take `&mut self` (both
        // preallocated; `take` swaps in an empty Vec, no allocation). The mono path discards
        // outbound (ADR-0026) — it has no out-parameter — so it renders into a throwaway sink.
        let mut master = std::mem::take(&mut self.master);
        let mut outbound = std::mem::take(&mut self.outbound_sink);
        outbound.clear();
        self.render_into(plan, messages, &mut master, &mut outbound);
        if let Some(ch0) = master.first() {
            out.copy_from_slice(&ch0[..out.len()]);
        } else {
            out.iter_mut().for_each(|s| *s = 0.0);
        }
        self.master = master;
        self.outbound_sink = outbound;
    }

    /// Render one block across **N logical master channels** (ADR-0026). `out` has one buffer
    /// per channel (`out.len() == plan.config.channels`), each `block_size` long. This is the
    /// stereo/multichannel path the engine drives. `outbound` receives any Messages an `osc_out`
    /// sink sent this block (ADR-0026); it is **appended to, never cleared** — the caller drains
    /// it. The boundary drain itself lands in phase 6; for now the parameter is preserved for the
    /// stable public signature. Allocation-free in steady state.
    pub fn render_block_multi(
        &mut self,
        plan: &mut Plan,
        messages: &[Message],
        out: &mut [Vec<f32>],
        outbound: &mut Vec<Message>,
    ) {
        self.render_into(plan, messages, out, outbound);
    }

    /// The shared render path: execute every node for the block and sum the master taps into
    /// `master` (one buffer per logical channel). `master.len()` should equal
    /// `plan.config.channels`; a tap addressing a channel beyond `master` is dropped.
    fn render_into(
        &mut self,
        plan: &mut Plan,
        messages: &[Message],
        master: &mut [Vec<f32>],
        _outbound: &mut Vec<Message>,
    ) {
        // Fresh edge buffers each block (upstream writes before downstream reads). Materialize
        // scratch buffers are excluded (ADR-0030): they are fully written by the materialize step
        // and persist a held input's value across blocks, so zeroing them would only force a
        // needless refill. `process_node` keeps each one fully defined (see `materialize_clean`).
        for (i, buf) in self.arena.iter_mut().enumerate() {
            if plan.materialize_scratch_mask[i] {
                continue;
            }
            buf.iter_mut().for_each(|s| *s = 0.0);
        }

        // Route external messages to node input ports, classified by port kind. Reuses scratch.
        // Operator-emitted messages are routed later, interleaved with execution.
        route_messages(&mut self.routes, plan, messages);
        self.emitted.clear();

        // Executor decides node order for the block (into reused capacity).
        self.executor.order(plan, &mut self.order);

        let sample_rate = plan.config.sample_rate;
        let block_size = self.block_size;

        // Disjoint field borrows so `process_node` can hold `arena` (inputs) and
        // `out_scratch` (outputs) at once while reading `routes`/`order`.
        let Self {
            arena,
            out_scratch,
            routes,
            emitted,
            emit_scratch,
            bounds,
            order,
            ..
        } = self;

        for &i in order.iter() {
            emit_scratch.clear();
            process_node(
                arena,
                out_scratch,
                bounds,
                &mut routes[i],
                &mut plan.nodes[i],
                messages,
                emitted,
                emit_scratch,
                sample_rate,
                block_size,
            );

            // Route this node's emissions (ADR-0014, ADR-0030): each goes into the block-lifetime
            // pool, and is delivered to every wired `(dst node, dst input port)` — which run later
            // in topo order, so they see it. The dst input port's [`PortKind`] decides how it
            // lands: a Held input latches + re-slices (the former context publish unifies here), a
            // Stream input gets a zero-copy event, a Dense input materializes ZOH.
            for e in emit_scratch.drain(..) {
                let port = e.port;
                let pool_idx = emitted.len();
                for &(dst, dst_port) in &plan.nodes[i].out_targets[port] {
                    let p = &plan.nodes[dst].descriptor.inputs[dst_port];
                    match port_kind(p) {
                        PortKind::Dense => {
                            if let Some(v) = e.arg.as_f32() {
                                routes[dst].materialize_writes.push((e.frame, dst_port, v));
                            }
                        }
                        PortKind::Held => {
                            if let Some(a) = held_arg(p, &e.arg) {
                                routes[dst].held.push((e.frame, dst_port, a));
                            }
                        }
                        PortKind::Stream => {
                            routes[dst].events.push(RoutedEvent {
                                dst_port,
                                src: EventSrc::Emitted(pool_idx),
                            });
                        }
                    }
                }
                emitted.push(e);
            }
        }

        // Sum master taps into the per-channel master (ADR-0026): every Lane of every tapped
        // port, in fixed order, so output stays deterministic (ADR-0001). A broadcast tap
        // (`channel: None`) adds to every channel — the historical mono fan, so channel 0 of a
        // fully-broadcast instrument is bit-identical to the pre-stereo single buffer. A
        // channel-pinned tap adds to that one channel only.
        for chan in master.iter_mut() {
            chan.iter_mut().for_each(|s| *s = 0.0);
        }
        for tap in &plan.output_taps {
            match tap.channel {
                None => {
                    for &buf in &tap.buffers {
                        for chan in master.iter_mut() {
                            for (o, s) in chan.iter_mut().zip(arena[buf].iter()) {
                                *o += *s;
                            }
                        }
                    }
                }
                Some(c) => {
                    if let Some(chan) = master.get_mut(c) {
                        for &buf in &tap.buffers {
                            for (o, s) in chan.iter_mut().zip(arena[buf].iter()) {
                                *o += *s;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Normalize a routed [`Arg`] for a **Held** input port (ADR-0030): clamp an `F32` to the port's
/// range, resolve an enum control message (symbol / index / variant) to the enum's concrete `Arg`,
/// or take any other vocab value (`Harmony`) as-is. `None` if it cannot be a value of this port.
fn held_arg(p: &Port, arg: &Arg) -> Option<Arg> {
    match &p.ty {
        PortType::F32 => arg
            .as_f32()
            .map(|v| Arg::F32(p.meta.as_ref().map(|m| m.clamp(v)).unwrap_or(v))),
        PortType::Vocab {
            enum_meta: Some(e), ..
        } => e.resolve_arg(arg),
        _ => Some(arg.clone()),
    }
}

/// Where a routed event's payload lives: an external block-input Message, or a Message an
/// upstream operator emitted this block (ADR-0014). Either way delivery is zero-copy — the
/// [`Event`] borrows the source.
#[derive(Clone, Copy)]
enum EventSrc {
    /// Index into the block `messages` slice, plus the byte offset where the node-local
    /// address begins (the external address is a full OSC path).
    External { msg: usize, local_start: usize },
    /// Index into the per-block emit pool. Its address is already node-local.
    Emitted(usize),
}

/// One routed Stream event: which dst input port it lands on, and where its payload lives.
/// Turning it into an [`Event`] at delivery allocates nothing.
struct RoutedEvent {
    dst_port: usize,
    src: EventSrc,
}

/// Per-node routed messages for one block (ADR-0030), one bucket per [`PortKind`].
#[derive(Default)]
struct NodeRoute {
    /// (frame, input port, value) — a scalar feeding a materialized [`Buffer`](PortType::Buffer)
    /// input. The engine writes it into the input's scratch buffer at its frame (ZOH); does **not**
    /// split the block. Sorted by frame before materialize.
    materialize_writes: Vec<(usize, usize, f32)>,
    /// (frame, input port, value) — a change to a **Held** input (ADR-0030): scalar / enum /
    /// `Harmony`. Splits the block (a held value is constant per `process` call); each interior
    /// frame becomes a segment boundary and updates the port's latch.
    held: Vec<(usize, usize, Arg)>,
    /// Routed **Stream** events (by reference), one per delivered Message — for event operators.
    events: Vec<RoutedEvent>,
}

/// Match each message to a node by address prefix, then to an input port by name, then classify
/// by the port's [`PortKind`]. Reuses `routes` (one entry per node); clears and refills.
fn route_messages(routes: &mut Vec<NodeRoute>, plan: &Plan, messages: &[Message]) {
    // Steady-state no-op: the Renderer presizes `routes` to the node count.
    if routes.len() != plan.nodes.len() {
        routes.resize_with(plan.nodes.len(), NodeRoute::default);
    }
    for r in routes.iter_mut() {
        r.materialize_writes.clear();
        r.held.clear();
        r.events.clear();
    }

    for (mi, msg) in messages.iter().enumerate() {
        for (i, node) in plan.nodes.iter().enumerate() {
            let Some(local) = local_address(&msg.address, &node.address) else {
                continue;
            };
            // Match the local address to an input port by name; deliver per the port's kind
            // (ADR-0030, Q11a — routing is by port, not by operator address-filtering).
            if let Some((port, p)) = node
                .descriptor
                .inputs
                .iter()
                .enumerate()
                .find(|(_, p)| p.name == local)
            {
                match port_kind(p) {
                    PortKind::Dense => {
                        if let Some(v) = msg.as_f32() {
                            routes[i].materialize_writes.push((msg.frame, port, v));
                        }
                    }
                    PortKind::Held => {
                        if let Some(a) = held_arg(p, &msg.arg) {
                            routes[i].held.push((msg.frame, port, a));
                        }
                    }
                    PortKind::Stream => {
                        let local_start = msg.address.len() - local.len();
                        routes[i].events.push(RoutedEvent {
                            dst_port: port,
                            src: EventSrc::External {
                                msg: mi,
                                local_start,
                            },
                        });
                    }
                }
            }
            break; // a message targets at most one node
        }
    }

    for r in routes.iter_mut() {
        r.materialize_writes.sort_by_key(|(f, _, _)| *f);
    }
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

/// Process one node for the block: materialize scalar-fed Buffer inputs, block-slice at Held
/// change frames, and within each segment run every Lane (Voice). Allocation-free in steady state:
/// per-Lane buffer wiring uses stack SmallVecs, output buffers are swapped through the reusable
/// `out_scratch`, and events are zero-copy.
#[allow(clippy::too_many_arguments)]
fn process_node(
    arena: &mut [Vec<f32>],
    out_scratch: &mut Vec<Vec<f32>>,
    bounds: &mut Vec<usize>,
    route: &mut NodeRoute,
    node: &mut PlanNode,
    messages: &[Message],
    emitted: &[Emit],
    emit_scratch: &mut Vec<Emit>,
    sample_rate: f32,
    block_size: usize,
) {
    // Emitted materialize writes are appended unsorted during execution; the ZOH fill needs them
    // in frame order.
    route.materialize_writes.sort_by_key(|(f, _, _)| *f);

    // Materialize each Buffer-from-scalar input into its scratch buffer (ADR-0030): fill from the
    // latched scalar, overwrite with each mid-block change from its frame onward, persist the final
    // value as the next block's latch, and flag `varying`. Done before the output-swap — scratch
    // buffers are inputs, disjoint from this node's outputs. These writes do NOT split the block;
    // sample-accuracy comes from writing them into the buffer at their frame.
    //
    // Cached steady state: a held-unchanged input is refilled only when it must be. Its scratch is
    // excluded from the per-block arena clear, so it persists; a constant block leaves it untouched.
    for k in 0..node.materialize.len() {
        let (port, buf) = node.materialize[k];
        let target = &mut arena[buf];
        let changed = route.materialize_writes.iter().any(|&(_, p, _)| p == port);
        if !changed {
            if !node.materialize_clean[k] {
                target[..block_size].fill(node.input_latches[port]);
                node.materialize_clean[k] = true;
            }
            node.varying[port] = false;
            continue;
        }
        let mut v = node.input_latches[port];
        let mut cursor = 0usize;
        for &(f, p, val) in &route.materialize_writes {
            if p != port {
                continue;
            }
            let f = f.min(block_size);
            target[cursor..f].fill(v);
            cursor = f;
            v = val;
        }
        target[cursor..block_size].fill(v);
        node.input_latches[port] = v;
        node.varying[port] = true;
        node.materialize_clean[k] = false;
    }

    // Segment boundaries: 0, block_size, every interior Held-change frame (a held value is constant
    // per `process` call, ADR-0030). Sort + dedup since change frames interleave.
    bounds.clear();
    bounds.push(0);
    bounds.push(block_size);
    for &(f, _, _) in &route.held {
        if f > 0 && f < block_size {
            bounds.push(f);
        }
    }
    bounds.sort_unstable();
    bounds.dedup();

    // Swap this node's signal-output buffers out of the arena into `out_scratch` (disjoint from
    // inputs — no self-loops; cycles error). Flat layout: index `port * lanes + lane`.
    out_scratch.clear();
    for port in &node.outputs {
        for &bi in port {
            out_scratch.push(std::mem::take(&mut arena[bi]));
        }
    }
    let lanes = node.lanes;
    let n_inputs = node.descriptor.inputs.len();

    for w in bounds.windows(2) {
        let (seg_start, seg_end) = (w[0], w[1]);
        if seg_start >= seg_end {
            continue;
        }

        // Apply Held changes landing at this segment's start: update the per-port latch, read by
        // every Lane via `io.last` (ADR-0030). Persists across blocks (the latch is next block's
        // frame-0 baseline). Last-write-wins on equal frames.
        for (f, port, arg) in route.held.iter() {
            if *f == seg_start {
                node.latch[*port] = arg.clone();
            }
        }

        // Per-input-port Stream events whose frame falls in this segment, as zero-copy views with
        // segment-relative frames (ADR-0030). Inline storage sized to the widest operator.
        let mut per_port: SmallVec<[SmallVec<[Event; 4]>; 24]> =
            (0..n_inputs).map(|_| SmallVec::new()).collect();
        for re in route.events.iter() {
            let (addr, arg, frame): (&str, &Arg, usize) = match re.src {
                EventSrc::External { msg, local_start } => {
                    let m = &messages[msg];
                    (&m.address[local_start..], &m.arg, m.frame)
                }
                EventSrc::Emitted(idx) => {
                    let e = &emitted[idx];
                    (e.address, &e.arg, e.frame)
                }
            };
            if frame >= seg_start && frame < seg_end {
                per_port[re.dst_port].push(Event {
                    address: addr,
                    arg,
                    frame: frame - seg_start,
                });
            }
        }
        let stream_refs: SmallVec<[&[Event]; 24]> = per_port.iter().map(|v| v.as_slice()).collect();

        for lane in 0..lanes {
            // Input slices for this Lane; a single-Lane source broadcasts.
            let inputs = node.inputs.iter().map(|src| {
                src.as_ref().map(|bufs| {
                    let bi = if bufs.len() == 1 { bufs[0] } else { bufs[lane] };
                    &arena[bi][seg_start..seg_end]
                })
            });

            // Output slices for this Lane: the strided entries of `out_scratch` whose
            // flat index has this lane (`port * lanes + lane`), in port order.
            let outputs = out_scratch
                .iter_mut()
                .enumerate()
                .filter(|(idx, _)| idx % lanes == lane)
                .map(|(_, buf)| &mut buf[seg_start..seg_end]);

            let io = Io::new(sample_rate, seg_end - seg_start, inputs, outputs)
                .with_latched(&node.latch)
                .with_streams(&stream_refs)
                .with_varying(&node.varying)
                .with_lane(lane, node.lanes);
            // Lane 0 collects emissions; their frames are stamped block-absolute by adding this
            // segment's start (ADR-0014). Other Lanes do not emit (single-Lane, pre-fan-out).
            let mut io = if lane == 0 {
                io.with_emit(&mut *emit_scratch, seg_start)
            } else {
                io
            };
            node.ops[lane].process(&mut io);
        }
    }

    // Return the signal-output buffers to the arena, same flat order they were taken.
    let mut k = 0;
    for port in &node.outputs {
        for &bi in port {
            arena[bi] = std::mem::take(&mut out_scratch[k]);
            k += 1;
        }
    }
}
