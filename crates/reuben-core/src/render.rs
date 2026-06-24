//! Render — executing a [`Plan`] per block (ADR-0009, ADR-0010, ADR-0011).
//!
//! The serial executor walks the topologically-ordered nodes. For each node, incoming
//! Messages are routed: those whose local address names a param drive **block-slicing**
//! (the block is split at their frames so [`Operator::process`] always sees a constant
//! param value); everything else is delivered as zero-copy [`Event`]s via [`Io::events`]
//! for event operators (the Voicer) to time themselves.
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

use crate::context::Context;
use crate::message::{Emit, Event, Message, Outbound};
use crate::operator::{CtxPublish, Io};
use crate::plan::{Plan, PlanNode};

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
    /// Persistent latched context per slot (ADR-0015): the value a follower reads at frame 0,
    /// carried across blocks. One per Context output port; init to the default context.
    context_arena: Vec<Context>,
    /// Block-lifetime pool of published context snapshots. Reader slices index it; grown
    /// once, cleared per block.
    context_pool: Vec<Context>,
    /// One node's context publishes for the current node, drained after it runs.
    ctx_publish_scratch: Vec<CtxPublish>,
    /// One node's outbound-route sends for the current node (ADR-0026), drained into the caller's
    /// outbound `Vec<Message>` (stamped with the node address) after it runs.
    outbound_scratch: Vec<Outbound>,
    /// Throwaway outbound sink for the mono [`Renderer::render_block`] convenience, which has no
    /// outbound out-parameter. Preallocated and cleared per call so render_block stays alloc-free.
    outbound_sink: Vec<Message>,
    /// Deferred per-slot baseline update: the `context_pool` index of this block's last
    /// publish to each slot, applied to `context_arena` at block end so pre-change segments
    /// still read the prior value. `usize::MAX` = no publish this block.
    ctx_pending: Vec<usize>,
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
            context_arena: vec![Context::default(); plan.num_context_slots],
            context_pool: Vec::with_capacity(EMIT_POOL_CAP),
            ctx_publish_scratch: Vec::with_capacity(EMIT_POOL_CAP),
            outbound_scratch: Vec::with_capacity(EMIT_POOL_CAP),
            outbound_sink: Vec::with_capacity(EMIT_POOL_CAP),
            ctx_pending: vec![usize::MAX; plan.num_context_slots],
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
    /// sink sent this block (ADR-0026), each stamped with its node's address; it is **appended to,
    /// never cleared** — the caller drains it (so an Engine can accumulate across several blocks of
    /// one callback). Allocation-free in steady state (a String per outbound Message when one flows).
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
        outbound: &mut Vec<Message>,
    ) {
        // Fresh edge buffers each block (upstream writes before downstream reads). Materialize
        // scratch buffers are excluded (ADR-0028): they are fully written by the materialize step
        // and persist a held Float input's value across blocks, so zeroing them would only force a
        // needless refill. `process_node` keeps each one fully defined (see `materialize_clean`).
        for (i, buf) in self.arena.iter_mut().enumerate() {
            if plan.materialize_scratch_mask[i] {
                continue;
            }
            buf.iter_mut().for_each(|s| *s = 0.0);
        }

        // Route external messages to nodes, split into param updates vs raw events. Reuses
        // scratch. Operator-emitted messages are routed later, interleaved with execution.
        route_messages(&mut self.routes, plan, messages);
        self.emitted.clear();
        self.context_pool.clear();

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
            context_arena,
            context_pool,
            ctx_publish_scratch,
            outbound_scratch,
            ctx_pending,
            bounds,
            order,
            ..
        } = self;

        for &i in order.iter() {
            emit_scratch.clear();
            ctx_publish_scratch.clear();
            outbound_scratch.clear();
            process_node(
                arena,
                out_scratch,
                bounds,
                &routes[i],
                &mut plan.nodes[i],
                messages,
                emitted,
                emit_scratch,
                context_arena,
                context_pool,
                ctx_publish_scratch,
                outbound_scratch,
                sample_rate,
                block_size,
            );

            // Drain this node's outbound sends (ADR-0026): stamp each with the node's address (the
            // outbound OSC address) and push past the boundary, appending to the caller's buffer.
            for o in outbound_scratch.drain(..) {
                outbound.push(Message {
                    addr: plan.nodes[i].address.clone(),
                    args: o.args,
                    frame: o.frame,
                });
            }

            // Route this node's emissions (ADR-0014): each goes into the block-lifetime
            // pool, and a zero-copy event reference is delivered to every downstream target
            // of its Message output port — which run later in topo order, so they see it.
            for e in emit_scratch.drain(..) {
                let port = e.port;
                let pool_idx = emitted.len();
                for &dst in &plan.nodes[i].msg_targets[port] {
                    routes[dst].events.push(RoutedEvent {
                        src: EventSrc::Emitted(pool_idx),
                    });
                }
                emitted.push(e);
            }

            // Route this node's context publishes (ADR-0015): snapshot into the block pool,
            // record a (frame, slot, pool_idx) reader-slice for every downstream reader, and
            // remember the last publish per slot for the deferred baseline update.
            for cp in ctx_publish_scratch.drain(..) {
                let slot = plan.nodes[i].context_outputs[cp.port];
                let pool_idx = context_pool.len();
                context_pool.push(cp.ctx);
                for &dst in &plan.nodes[i].ctx_targets[cp.port] {
                    routes[dst].context.push((cp.frame, slot, pool_idx));
                }
                ctx_pending[slot] = pool_idx;
            }
        }

        // Deferred baseline: a slot's persistent value becomes this block's last publish, so
        // next block's frame-0 readers see it — while this block's pre-change segments still
        // read the prior value (they consult the pool, not the baseline, past a publish).
        for (slot, p) in ctx_pending.iter_mut().enumerate() {
            if *p != usize::MAX {
                context_arena[slot] = context_pool[*p];
                *p = usize::MAX;
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

/// One routed event: where its payload lives. Turning it into an [`Event`] at delivery
/// allocates nothing.
struct RoutedEvent {
    src: EventSrc,
}

/// Per-node routed messages for one block.
#[derive(Default)]
struct NodeRoute {
    /// (frame, param slot, value) — drive block-slicing.
    params: Vec<(usize, usize, f32)>,
    /// (frame, input port, value) — a change to a materialized [`Shape::Float`] input
    /// (ADR-0028). Unlike `params` these do **not** split the block; the engine writes them into
    /// the input's materialized buffer at their frame, so a per-sample reader sees them
    /// sample-accurately in one `process` call. Sorted by frame.
    floats: Vec<(usize, usize, f32)>,
    /// (frame, input port, variant index) — a change to an [`Shape::Enum`] input (ADR-0028). Like
    /// `params` these **split** the block (a held discrete choice is constant per `process` call):
    /// each interior frame becomes a segment boundary and updates the port's enum latch. Sorted
    /// by frame.
    enums: Vec<(usize, usize, usize)>,
    /// Routed events (by reference into the block's Messages) — for event operators.
    events: Vec<RoutedEvent>,
    /// (frame, context slot, pool index) — a context change a follower reads; each frame
    /// also becomes a slice boundary (ADR-0015). Filled by the publish drain, not by
    /// `route_messages`.
    context: Vec<(usize, usize, usize)>,
}

/// Match each message to a node by address prefix, then classify param vs event.
/// Reuses `routes` (one entry per node); clears the inner Vecs and refills them.
fn route_messages(routes: &mut Vec<NodeRoute>, plan: &Plan, messages: &[Message]) {
    // Steady-state no-op: the Renderer presizes `routes` to the node count.
    if routes.len() != plan.nodes.len() {
        routes.resize_with(plan.nodes.len(), NodeRoute::default);
    }
    for r in routes.iter_mut() {
        r.params.clear();
        r.floats.clear();
        r.enums.clear();
        r.events.clear();
        r.context.clear();
    }

    for (mi, msg) in messages.iter().enumerate() {
        for (i, node) in plan.nodes.iter().enumerate() {
            let Some(local) = local_address(&msg.addr, &node.address) else {
                continue;
            };
            // A new-style materialized Float input takes precedence: its value rides the
            // materialize buffer (written at `frame`), not a block-slicing param (ADR-0028).
            if let Some((port, meta)) = node.descriptor.materialized_input(local) {
                if let Some(v) = msg.first_f32() {
                    routes[i].floats.push((msg.frame, port, meta.clamp(v)));
                }
                break;
            }
            // An Enum input resolves its first arg as a wire token (symbol `"Hp"` or fallback
            // index `"1"`) to a held variant index; like a param it splits the block (ADR-0028).
            if let Some((port, e)) = node.descriptor.enum_input(local) {
                if let Some(idx) = msg.args.first().and_then(|a| e.resolve_arg(a)) {
                    routes[i].enums.push((msg.frame, port, idx));
                }
                break;
            }
            match node.descriptor.param_index(local) {
                Some(slot) => {
                    if let Some(v) = msg.first_f32() {
                        let v = node.descriptor.params[slot].clamp(v);
                        routes[i].params.push((msg.frame, slot, v));
                    }
                }
                None => {
                    // `local` is a suffix of `msg.addr`; record where it starts.
                    let local_start = msg.addr.len() - local.len();
                    routes[i].events.push(RoutedEvent {
                        src: EventSrc::External {
                            msg: mi,
                            local_start,
                        },
                    });
                }
            }
            break; // a message targets at most one node
        }
    }
    for r in routes.iter_mut() {
        r.params.sort_by_key(|(f, _, _)| *f);
        r.floats.sort_by_key(|(f, _, _)| *f);
        r.enums.sort_by_key(|(f, _, _)| *f);
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

/// Process one node for the block: block-slice at param frames, and within each segment
/// run every Lane (Voice). Allocation-free: per-Lane buffer wiring uses stack SmallVecs,
/// output buffers are swapped through the reusable `out_scratch`, and events are zero-copy.
#[allow(clippy::too_many_arguments)]
fn process_node(
    arena: &mut [Vec<f32>],
    out_scratch: &mut Vec<Vec<f32>>,
    bounds: &mut Vec<usize>,
    route: &NodeRoute,
    node: &mut PlanNode,
    messages: &[Message],
    emitted: &[Emit],
    emit_scratch: &mut Vec<Emit>,
    context_arena: &[Context],
    context_pool: &[Context],
    ctx_publish_scratch: &mut Vec<CtxPublish>,
    outbound_scratch: &mut Vec<Outbound>,
    sample_rate: f32,
    block_size: usize,
) {
    let params = &route.params;

    // Materialize each unwired Float input into its scratch buffer (ADR-0028): fill from the
    // latched scalar, overwrite with each mid-block change from its frame onward, persist the
    // final value as the next block's latch, and flag `varying` (changed this block). Done before
    // the output-swap — scratch buffers are inputs, disjoint from this node's outputs. Float
    // changes do NOT split the block; sample-accuracy comes from writing them into the buffer at
    // their frame.
    //
    // `node.varying` is preallocated at instantiate (length = input count) and reused — the audio
    // thread never allocates it, even for an operator with >8 inputs. It is all-`true` to start;
    // only materialized ports are rewritten here, so legacy / wired ports keep `true` (the same
    // conservative default `Io::varying` reports for an unattached slice).
    //
    // Cached steady state (ADR-0028): a held-unchanged Float input is refilled only when it must
    // be. Its scratch buffer is excluded from the per-block arena clear, so it persists; a constant
    // block leaves it untouched (steady-state ~nil work). We refill only on a mid-block change, or
    // the one block after a change/at startup that must re-flatten the buffer to the latch
    // (`materialize_clean`). Output is identical to rewriting every block.
    for k in 0..node.materialize.len() {
        let (port, buf) = node.materialize[k];
        let target = &mut arena[buf];
        let changed = route.floats.iter().any(|&(_, p, _)| p == port);
        if !changed {
            // Held constant this block. Re-flatten to the latch only if the buffer is not already
            // uniformly it (first block, or a prior block left a mid-block gradient); otherwise the
            // persisted buffer is already correct and we touch nothing.
            if !node.materialize_clean[k] {
                target[..block_size].fill(node.input_latches[port]);
                node.materialize_clean[k] = true;
            }
            node.varying[port] = false;
            continue;
        }
        // Changed: fill from the latch, overwriting with each mid-block change from its frame
        // onward; persist the final value as the next block's latch. The buffer now holds a
        // gradient, so the next constant block must re-flatten it.
        let mut v = node.input_latches[port];
        let mut cursor = 0usize;
        for &(f, p, val) in &route.floats {
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

    // Segment boundaries: 0, block_size, every interior param frame, every interior enum-change
    // frame (ADR-0028 — a held discrete choice is constant per call), and every interior context
    // change frame (ADR-0015 — so a chord/key change splits the block and the follower reads the
    // right context per segment). Sort + dedup since these frames interleave.
    bounds.clear();
    bounds.push(0);
    bounds.push(block_size);
    for &(f, _, _) in params {
        if f > 0 && f < block_size {
            bounds.push(f);
        }
    }
    for &(f, _, _) in &route.enums {
        if f > 0 && f < block_size {
            bounds.push(f);
        }
    }
    for &(f, _, _) in &route.context {
        if f > 0 && f < block_size {
            bounds.push(f);
        }
    }
    bounds.sort_unstable();
    bounds.dedup();

    // Swap this node's output buffers out of the arena into `out_scratch` (disjoint from
    // inputs — no self-loops; cycles error). Flat layout: index `port * lanes + lane`.
    out_scratch.clear();
    for port in &node.outputs {
        for &bi in port {
            out_scratch.push(std::mem::take(&mut arena[bi]));
        }
    }
    let lanes = node.lanes;

    for w in bounds.windows(2) {
        let (seg_start, seg_end) = (w[0], w[1]);
        if seg_start >= seg_end {
            continue;
        }

        // Apply enum updates landing at this segment's start: latch the held variant index, read
        // by every Lane via `io.enum_index` (ADR-0028). Persists across blocks.
        for &(f, port, idx) in &route.enums {
            if f == seg_start {
                node.enum_latches[port] = idx;
            }
        }

        // Apply param updates landing at this segment's start (shared across Lanes).
        for &(f, slot, v) in params {
            if f == seg_start {
                node.params[slot] = v;
            }
        }

        // Resolve the Context for each Context input port at this segment's start (ADR-0015):
        // the latest publish with frame ≤ seg_start (last-write-wins on equal frames), else
        // the persistent baseline. Constant across the segment and across Lanes.
        let mut seg_contexts: SmallVec<[Context; 2]> = SmallVec::new();
        for &slot_opt in &node.context_inputs {
            let ctx = match slot_opt {
                None => Context::default(),
                Some(slot) => {
                    let mut best: Option<(usize, usize)> = None; // (frame, pool_idx)
                    for &(f, s, pidx) in &route.context {
                        if s == slot && f <= seg_start {
                            let take =
                                best.is_none_or(|(bf, bi)| f > bf || (f == bf && pidx >= bi));
                            if take {
                                best = Some((f, pidx));
                            }
                        }
                    }
                    match best {
                        Some((_, pidx)) => context_pool[pidx],
                        None => context_arena[slot],
                    }
                }
            };
            seg_contexts.push(ctx);
        }

        // Events whose frame falls in this segment, as zero-copy views with
        // segment-relative frames. Inline storage — no heap for the common small case.
        // The payload is an external block Message or an upstream-emitted one (ADR-0014).
        let mut seg_events: SmallVec<[Event; 8]> = SmallVec::new();
        for re in &route.events {
            let (addr, args, frame): (&str, &crate::message::Args, usize) = match re.src {
                EventSrc::External { msg, local_start } => {
                    let m = &messages[msg];
                    (&m.addr[local_start..], &m.args, m.frame)
                }
                EventSrc::Emitted(idx) => {
                    let e = &emitted[idx];
                    (e.addr, &e.args, e.frame)
                }
            };
            if frame >= seg_start && frame < seg_end {
                seg_events.push(Event {
                    addr,
                    args,
                    frame: frame - seg_start,
                });
            }
        }

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

            let io = Io::new(
                sample_rate,
                seg_end - seg_start,
                inputs,
                outputs,
                &node.params,
                &seg_events,
            )
            .with_lane(lane, node.lanes)
            .with_contexts(&seg_contexts)
            .with_varying(&node.varying)
            .with_enums(&node.enum_latches);
            // Lane 0 collects emissions and context publishes; their frames are stamped
            // block-absolute by adding this segment's start (ADR-0014, ADR-0015). Other Lanes
            // neither emit nor publish (single-Lane, pre-fan-out).
            let mut io = if lane == 0 {
                io.with_emit(&mut *emit_scratch, seg_start)
                    .with_context_publish(&mut *ctx_publish_scratch, seg_start)
                    .with_outbound(&mut *outbound_scratch, seg_start)
            } else {
                io
            };
            node.ops[lane].process(&mut io);
        }
    }

    // Return the output buffers to the arena, same flat order they were taken.
    let mut k = 0;
    for port in &node.outputs {
        for &bi in port {
            arena[bi] = std::mem::take(&mut out_scratch[k]);
            k += 1;
        }
    }
}
