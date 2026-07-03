//! Render — executing a [`Plan`] per block (ADR-0009, ADR-0010, ADR-0011, ADR-0030).
//!
//! The serial executor walks the topologically-ordered nodes. For each node, incoming
//! Messages are routed to its input ports by the port's [`PortKind`](crate::plan::PortKind):
//! a **Value** control (scalar / enum / `Harmony`) drives **block-slicing** (the block is split
//! at its change frames so [`Operator::process`] always sees a constant held value, read via
//! `io.input::<T>`); an **Event** (`Note`) is delivered as a zero-copy [`Event`] on its port
//! (read via `io.input::<Note>`); a **Signal** [`Buffer`] input fed by a scalar is **materialized** ZOH
//! into its arena buffer (read via `io.input::<&[f32]>`).
//!
//! Polyphony is hosted inside the Voicer (N voice sub-plans summed), not fanned out across the
//! engine — the retired Lane model (ADR-0032).
//!
//! **Realtime-safe.** A [`Renderer`] preallocates its edge-buffer arena and every piece
//! of per-block scratch at construction, and reuses them; steady-state [`Renderer::render_block`]
//! performs no heap allocation (verified by `tests/rt_safe.rs`). Per-port buffer wiring
//! uses stack [`SmallVec`]s; routed events are zero-copy views onto the caller's Messages.
//!
//! The [`Executor`] trait is the pluggable-executor seam (ADR-0001).

use smallvec::SmallVec;

use crate::descriptor::{Port, PortType};
use crate::message::{Arg, Emit, Event, Message};
use crate::operator::Io;
use crate::plan::{InterfaceOutput, Plan, PlanNode, PortKind};

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

/// Reusable per-block render scratch (ADR-0030, ADR-0032 §4): everything the render path needs
/// *besides* the edge-buffer arena. Preallocated once (sized to a plan) and reused, so the render
/// path is allocation-free in steady state. Kept separate from the arena so [`render_plan`] is a
/// re-entrant free function — a hosting operator (`Voicer`) renders each sub-plan with that
/// sub-plan's own arena while reusing one shared `RenderScratch`.
pub struct RenderScratch {
    /// Per-node scratch: the current node's signal-output buffers, swapped out of `arena` so
    /// inputs (still in `arena`) and outputs are disjointly borrowable. In signal-output port
    /// order. Cleared and refilled per node — capacity retained.
    out_scratch: Vec<Vec<f32>>,
    /// Per-block message routing, one entry per node. Reused; inner Vecs cleared per block.
    routes: Vec<NodeRoute>,
    /// Block-lifetime pool of operator-emitted Messages (ADR-0014). Routed events borrow
    /// from it, so it is grown once and only cleared (never freed) per block.
    emitted: Vec<Emit>,
    /// One node's emissions for the current node, drained into `emitted` after it runs.
    emit_scratch: Vec<Emit>,
    /// Per-node block-slice boundaries scratch. Cleared per node — capacity retained.
    bounds: Vec<usize>,
    /// Execution order for the block, refilled by the executor into reused capacity.
    order: Vec<usize>,
}

impl RenderScratch {
    /// Preallocate scratch sized to `plan` so the steady-state render path never grows it.
    pub fn new(plan: &Plan) -> Self {
        let routes = (0..plan.nodes.len())
            .map(|_| NodeRoute::default())
            .collect();
        let max_out_bufs = plan
            .nodes
            .iter()
            .map(|n| n.outputs.iter().map(|p| p.len()).sum::<usize>())
            .max()
            .unwrap_or(0);
        Self {
            out_scratch: Vec::with_capacity(max_out_bufs),
            routes,
            emitted: Vec::with_capacity(EMIT_POOL_CAP),
            emit_scratch: Vec::with_capacity(EMIT_POOL_CAP),
            bounds: Vec::with_capacity(8),
            order: Vec::with_capacity(plan.nodes.len()),
        }
    }
}

/// Owns the edge-buffer arena and drives Render for a single Plan.
///
/// All buffers and per-block scratch are allocated once here and reused, so
/// [`Renderer::render_block`] is allocation-free in steady state.
pub struct Renderer<E: Executor = SerialExecutor> {
    /// Edge buffers, indexed by arena slot; one block long each.
    arena: Vec<Vec<f32>>,
    /// Reusable per-block scratch (everything but the arena), shared across re-entrant renders.
    scratch: RenderScratch,
    /// Throwaway outbound sink for the mono [`Renderer::render_block`] convenience, which has no
    /// outbound out-parameter. Preallocated and cleared per call so render_block stays alloc-free.
    outbound_sink: Vec<Message>,
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

        Self {
            arena,
            scratch: RenderScratch::new(plan),
            outbound_sink: Vec::with_capacity(EMIT_POOL_CAP),
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
    /// sink sent this block (ADR-0026, ADR-0030); it is **appended to, never cleared** — the caller
    /// drains it. Allocation-free in steady state.
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
    /// `plan.config.channels`; a tap addressing a channel beyond `master` is dropped. Delegates to
    /// the re-entrant [`render_plan`] over this renderer's owned arena + scratch.
    fn render_into(
        &mut self,
        plan: &mut Plan,
        messages: &[Message],
        master: &mut [Vec<f32>],
        outbound: &mut Vec<Message>,
    ) {
        render_plan(
            plan,
            &mut self.arena,
            &mut self.scratch,
            &self.executor,
            messages,
            self.block_size,
            master,
            outbound,
        );
    }
}

/// Re-entrant block render over an explicit `(plan, arena, scratch)` (ADR-0032 §4): execute every
/// node for `frames` frames and sum the master taps into `master` (one buffer per logical channel,
/// `master.len()` should equal `plan.config.channels`; a tap beyond `master` is dropped). `outbound`
/// is **appended to** (ADR-0026) — the caller drains it.
///
/// This is the primitive that makes render re-entrant: render is a pure function of
/// `(plan, arena, scratch)`, not nested mutable renderer state. The top-level [`Renderer`] calls it
/// with the rig's arena; a hosting operator (`Voicer`, ADR-0032) calls the *same* function per
/// active voice with that voice's own arena, reusing one shared `RenderScratch`. Allocation-free in
/// steady state when `scratch` was [`RenderScratch::new`]-sized to `plan`.
/// The [`Plan::captured`] slot a Value `interface` output `(node, port)` writes to, or `None` if the
/// emitting port is not a captured Value boundary output. Linear scan over the (tiny) interface list.
fn capture_slot(outs: &[InterfaceOutput], node: usize, port: usize) -> Option<usize> {
    outs.iter()
        .find(|o| o.node == node && o.port == port)
        .and_then(|o| o.captured_slot)
}

#[allow(clippy::too_many_arguments)]
pub fn render_plan<E: Executor>(
    plan: &mut Plan,
    arena: &mut [Vec<f32>],
    scratch: &mut RenderScratch,
    executor: &E,
    messages: &[Message],
    frames: usize,
    master: &mut [Vec<f32>],
    outbound: &mut Vec<Message>,
) {
    // Fresh edge buffers each block (upstream writes before downstream reads). Materialize
    // scratch buffers are excluded (ADR-0030): they are fully written by the materialize step
    // and persist a held input's value across blocks, so zeroing them would only force a
    // needless refill. `process_node` keeps each one fully defined (see `materialize_clean`).
    for (i, buf) in arena.iter_mut().enumerate() {
        if plan.materialize_scratch_mask[i] {
            continue;
        }
        buf.iter_mut().for_each(|s| *s = 0.0);
    }

    // Route external messages to node input ports, classified by port kind. Reuses scratch.
    // Operator-emitted messages are routed later, interleaved with execution.
    route_messages(&mut scratch.routes, plan, messages);
    scratch.emitted.clear();

    // Executor decides node order for the block (into reused capacity).
    executor.order(plan, &mut scratch.order);

    let sample_rate = plan.config.sample_rate;
    let block_size = frames;

    // Disjoint field borrows so `process_node` can hold `arena` (inputs) and
    // `out_scratch` (outputs) at once while reading `routes`/`order`.
    let RenderScratch {
        out_scratch,
        routes,
        emitted,
        emit_scratch,
        bounds,
        order,
    } = scratch;

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

        // An `osc_out` sink: its emissions leave the graph (ADR-0026, ADR-0030). Drain each to
        // the outbound list stamped with the node's (fixed) address and the already-block-
        // absolute frame; native encodes + sends them. A sink has no downstream wiring, so this
        // replaces — not supplements — the routing below.
        if let Some(addr) = plan
            .outbound_taps
            .iter()
            .find(|t| t.node == i)
            .map(|t| t.address.as_str())
        {
            for e in emit_scratch.drain(..) {
                outbound.push(Message::new(addr, e.arg, e.frame));
            }
            continue;
        }

        // Route this node's emissions (ADR-0014, ADR-0030): each goes into the block-lifetime
        // pool, and is delivered to every wired `(dst node, dst input port)` — which run later
        // in topo order, so they see it. The dst input port's [`PortKind`] decides how it
        // lands: a Value input latches + re-slices (the former context publish unifies here), a
        // Event input gets a zero-copy event, a Signal input materializes ZOH.
        for e in emit_scratch.drain(..) {
            let port = e.port;
            let pool_idx = emitted.len();
            // Capture a Value `interface` output (ADR-0032 §4): its last-emitted scalar is held for
            // the host to read post-render. The port may also be wired downstream — capture is
            // additive (a tap), not a replacement for routing below.
            if let Some(slot) = capture_slot(&plan.interface_outputs, i, port) {
                if let Some(v) = e.arg.as_f32() {
                    plan.captured[slot] = v;
                }
            }
            for &(dst, dst_port) in &plan.nodes[i].out_targets[port] {
                match plan.nodes[dst].input_kinds[dst_port] {
                    PortKind::Signal => {
                        if let Some(v) = e.arg.as_f32() {
                            routes[dst].materialize_writes.push((e.frame, dst_port, v));
                        }
                    }
                    PortKind::Value => {
                        let p = &plan.nodes[dst].descriptor.inputs[dst_port];
                        if let Some(a) = held_arg(p, &e.arg) {
                            routes[dst].held.push((e.frame, dst_port, a));
                        }
                    }
                    PortKind::Event => {
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

    // Sum master taps into the per-channel master (ADR-0026): every buffer of every tapped
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

/// The single-node injection + observation seam (`OpDriver`, in-crate test/bench harness). Drives
/// **one** node for `frames` frames through the *real* engine substrate — the same per-block edge
/// clear, [`route_messages`], and [`process_node`] the render loop runs — so a harness over it can
/// never drift from production seeding/stepping. `pub(crate)` and gated to test/bench builds: it is
/// not part of the public render API, but [`crate::op_driver`] reaches it from inside the crate.
#[cfg(any(test, feature = "bench"))]
impl<E: Executor> Renderer<E> {
    /// Step node `node_idx` for `frames` frames (≤ this renderer's `block_size`). Clears the edge
    /// arena (preserving materialize scratch — which is where a driven audio-in buffer lives, so it
    /// survives the clear), routes `messages` to input ports, and runs [`process_node`]. Read the
    /// node's output buffers with [`Renderer::arena_buffer`] and its emissions with
    /// [`Renderer::last_emits`] afterward.
    pub(crate) fn step_node(
        &mut self,
        plan: &mut Plan,
        node_idx: usize,
        frames: usize,
        messages: &[Message],
    ) {
        // Same per-block fresh-edge clear as `render_plan` (materialize scratch excepted).
        for (i, buf) in self.arena.iter_mut().enumerate() {
            if plan.materialize_scratch_mask[i] {
                continue;
            }
            buf.iter_mut().for_each(|s| *s = 0.0);
        }
        route_messages(&mut self.scratch.routes, plan, messages);
        self.scratch.emitted.clear();
        let sample_rate = plan.config.sample_rate;

        let arena = &mut self.arena;
        let RenderScratch {
            out_scratch,
            routes,
            emitted,
            emit_scratch,
            bounds,
            ..
        } = &mut self.scratch;
        emit_scratch.clear();
        process_node(
            arena,
            out_scratch,
            bounds,
            &mut routes[node_idx],
            &mut plan.nodes[node_idx],
            messages,
            emitted,
            emit_scratch,
            sample_rate,
            frames,
        );
    }

    /// Read edge-arena buffer `bi` (a node output, or a driven input's scratch). Length `block_size`.
    pub(crate) fn arena_buffer(&self, bi: usize) -> &[f32] {
        &self.arena[bi]
    }

    /// Mutable edge-arena buffer `bi`, for writing a driven (time-varying audio-in) buffer before a
    /// [`Renderer::step_node`] call. The slot must be materialize scratch so the per-block clear skips it.
    pub(crate) fn arena_buffer_mut(&mut self, bi: usize) -> &mut [f32] {
        &mut self.arena[bi]
    }

    /// The emissions the last [`Renderer::step_node`] collected (segment-relative frames).
    pub(crate) fn last_emits(&self) -> &[Emit] {
        &self.scratch.emit_scratch
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
    /// Index into the block `messages` slice. The event delivers by the wired input port, so only
    /// the payload (and frame) are carried forward — the external address is not (ADR-0031 step 7).
    External { msg: usize },
    /// Index into the per-block emit pool.
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
    /// (frame, input port, value) — a scalar feeding a materialized [`Buffer`](PortType::F32Buffer)
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
        // Resolve the address to its one destination port ([`resolve_port`], shared with the
        // boundary's [`Plan::osc_in_message`]); deliver per the port's kind (ADR-0030, Q11a —
        // routing is by port, not by operator address-filtering).
        let Some((i, port, p)) = resolve_port(&plan.nodes, &msg.address) else {
            continue;
        };
        match plan.nodes[i].input_kinds[port] {
            PortKind::Signal => {
                if let Some(v) = msg.as_f32() {
                    routes[i].materialize_writes.push((msg.frame, port, v));
                }
            }
            PortKind::Value => {
                if let Some(a) = held_arg(p, &msg.arg) {
                    routes[i].held.push((msg.frame, port, a));
                }
            }
            PortKind::Event => {
                routes[i].events.push(RoutedEvent {
                    dst_port: port,
                    src: EventSrc::External { msg: mi },
                });
            }
        }
    }

    for r in routes.iter_mut() {
        r.materialize_writes.sort_by_key(|(f, _, _)| *f);
    }
}

/// Resolve an inbound address to its destination — `(node index, input port index, port)` —
/// by matching a node address prefix ([`local_address`]) and then an input port by name. The
/// **single** "address → node + port" rule: both [`route_messages`] and the boundary's
/// [`Plan::osc_in_message`] call it, so the two inbound paths cannot diverge (issue #165 —
/// a diverged copy silently dropped messages to nested nodes).
///
/// A node whose address prefix-matches but has **no** matching port does not decide the
/// outcome — keep scanning. Node addresses may be ancestors of one another (`/fx` beside an
/// inlined `/fx/verb/delay`, ADR-0034 §3 manufactures these systematically), and
/// `/fx/verb/delay/time` must reach the deeper node even though `/fx` prefix-matches it
/// first in plan order.
pub(crate) fn resolve_port<'p>(
    nodes: &'p [PlanNode],
    address: &str,
) -> Option<(usize, usize, &'p Port)> {
    for (i, node) in nodes.iter().enumerate() {
        let Some(local) = local_address(address, &node.address) else {
            continue;
        };
        if let Some((pi, port)) = node
            .descriptor
            .inputs
            .iter()
            .enumerate()
            .find(|(_, p)| p.name == local)
        {
            return Some((i, pi, port)); // a message targets exactly one port
        }
    }
    None
}

/// Local address of `addr` relative to `node_addr`, if `addr` targets that node.
/// `/osc/freq` under `/osc` -> `freq`; `/osc` under `/osc` -> `` (whole-node).
pub(crate) fn local_address<'a>(addr: &'a str, node_addr: &str) -> Option<&'a str> {
    if addr == node_addr {
        return Some("");
    }
    let rest = addr.strip_prefix(node_addr)?;
    rest.strip_prefix('/')
}

/// Process one node for the block: materialize scalar-fed Buffer inputs, block-slice at Held
/// change frames, and run the operator on each segment. Allocation-free in steady state:
/// per-port buffer wiring uses stack SmallVecs, output buffers are swapped through the reusable
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
    // value as the next block's latch, and flag `varying`. Unwired inputs materialize too — a bare
    // buffer's latch seeds 0.0, so it fills with silence — which is what upholds the
    // buffer-presence invariant (ADR-0037): every Signal input reaches `process` as a dense
    // length-n slice. Done before the output-swap — scratch buffers are inputs, disjoint from this
    // node's outputs. These writes do NOT split the block; sample-accuracy comes from writing them
    // into the buffer at their frame.
    //
    // Cached steady state: a held-unchanged input is refilled only when it must be. Its scratch is
    // excluded from the per-block arena clear, so it persists; a constant block leaves it untouched.
    for k in 0..node.materialize.len() {
        let (port, buf) = node.materialize[k];
        let target = &mut arena[buf];
        let changed = route.materialize_writes.iter().any(|&(_, p, _)| p == port);
        // The latch is the sole ZOH store (ADR-0030): a materialized port is always seeded/set
        // `Arg::F32`, so this decode succeeds — assert it loudly in dev, hold the additive identity
        // in release if a wrong-typed port ever reaches here.
        debug_assert!(
            node.latch[port].as_f32().is_some(),
            "materialized port {port} must latch a numeric Arg"
        );
        if !changed {
            if !node.materialize_clean[k] {
                target[..block_size].fill(node.latch[port].as_f32().unwrap_or(0.0));
                node.materialize_clean[k] = true;
            }
            node.varying[port] = false;
            continue;
        }
        let mut v = node.latch[port].as_f32().unwrap_or(0.0);
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
        // Persist the end-of-block value as the next block's ZOH (ADR-0030): `latch` is the single
        // source of truth — the materialized buffer is the sample-accurate path, the latch the
        // `io.input::<T>` read. A Buffer port carries no meaningful held value, so the write is ignored there.
        node.latch[port] = Arg::F32(v);
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
    // inputs — no self-loops; cycles error), in signal-output port order.
    out_scratch.clear();
    for port in &node.outputs {
        for &bi in port {
            out_scratch.push(std::mem::take(&mut arena[bi]));
        }
    }
    let n_inputs = node.descriptor.inputs.len();

    for w in bounds.windows(2) {
        let (seg_start, seg_end) = (w[0], w[1]);
        if seg_start >= seg_end {
            continue;
        }

        // Apply Held changes landing at this segment's start: update the per-port latch, read via
        // `io.input::<T>` (ADR-0030). Persists across blocks (the latch is next block's frame-0 baseline).
        // Last-write-wins on equal frames.
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
            let (arg, frame): (&Arg, usize) = match re.src {
                EventSrc::External { msg } => {
                    let m = &messages[msg];
                    (&m.arg, m.frame)
                }
                EventSrc::Emitted(idx) => {
                    let e = &emitted[idx];
                    (&e.arg, e.frame)
                }
            };
            if frame >= seg_start && frame < seg_end {
                per_port[re.dst_port].push(Event {
                    arg,
                    frame: frame - seg_start,
                });
            }
        }
        let stream_refs: SmallVec<[&[Event]; 24]> = per_port.iter().map(|v| v.as_slice()).collect();

        // Input slices for this segment.
        let inputs = node
            .inputs
            .iter()
            .map(|src| src.as_ref().map(|bufs| &arena[bufs[0]][seg_start..seg_end]));

        // Output slices for this segment, in signal-output port order.
        let outputs = out_scratch
            .iter_mut()
            .map(|buf| &mut buf[seg_start..seg_end]);

        // Emitted frames are stamped block-absolute by adding this segment's start (ADR-0014).
        let mut io = Io::new(sample_rate, seg_end - seg_start, inputs, outputs)
            .with_latched(&node.latch)
            .with_streams(&stream_refs)
            .with_varying(&node.varying)
            .with_emit(&mut *emit_scratch, seg_start);
        node.ops[0].process(&mut io);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{load, AudioConfig, Registry};

    /// Two leaf nodes in an ancestor-prefix relationship — the shape ADR-0034 §3's inlining
    /// manufactures systematically (`/fx` beside an inlined `/fx/verb/delay`). The wire from
    /// `/fx` pins the topo order, so the portless ancestor is genuinely scanned first.
    fn shadowed_plan() -> Plan {
        const SHADOWED: &str = r#"{
            "instrument": "shadowed",
            "nodes": [
                { "type": "oscillator", "address": "/fx" },
                { "type": "delay", "address": "/fx/verb/delay",
                  "inputs": { "audio": { "from": "/fx.audio" } } }
            ],
            "outputs": [ { "node": "/fx/verb/delay", "port": "audio" } ]
        }"#;
        let graph = load(SHADOWED, &Registry::builtin()).expect("load");
        Plan::instantiate(graph, AudioConfig::new(48_000.0, 64)).expect("instantiate")
    }

    /// Issue #165: inbound OSC to a nested node must not be dropped because a shallower
    /// prefix-matching node that lacks the port sits earlier in `plan.nodes`.
    #[test]
    fn osc_in_message_reaches_a_node_shadowed_by_a_portless_ancestor() {
        let plan = shadowed_plan();
        let fx = plan
            .nodes
            .iter()
            .position(|n| n.address == "/fx")
            .expect("/fx node");
        let deep = plan
            .nodes
            .iter()
            .position(|n| n.address == "/fx/verb/delay")
            .expect("/fx/verb/delay node");
        assert!(
            fx < deep,
            "precondition broken: /fx must precede /fx/verb/delay in plan order"
        );

        let msg = plan
            .osc_in_message("/fx/verb/delay/time", &[Arg::F32(0.5)])
            .expect("message to the nested node was dropped at the boundary");
        assert_eq!(msg.address, "/fx/verb/delay/time");
        assert_eq!(msg.arg, Arg::F32(0.5));
        assert_eq!(msg.frame, 0);
    }

    /// Parity guard: the boundary conversion (`osc_in_message`) and the render routing
    /// (`route_messages`) agree on the destination node. Both call [`resolve_port`]; this
    /// pins them together behaviorally if either ever re-inlines the rule (issue #165).
    #[test]
    fn osc_in_message_and_route_messages_agree_on_the_destination() {
        let plan = shadowed_plan();
        let cases = [
            // A direct hit on the ancestor (Signal input: oscillator freq is a signal control).
            ("/fx/freq", "/fx"),
            // Nested targets behind the portless prefix (Value inputs).
            ("/fx/verb/delay/time", "/fx/verb/delay"),
            ("/fx/verb/delay/mix", "/fx/verb/delay"),
        ];
        let mut routes = Vec::new();
        for (addr, want) in cases {
            let msg = plan
                .osc_in_message(addr, &[Arg::F32(0.25)])
                .unwrap_or_else(|| panic!("{addr}: the boundary dropped the message"));
            route_messages(&mut routes, &plan, &[msg]);
            let delivered: Vec<&str> = routes
                .iter()
                .zip(&plan.nodes)
                .filter(|(r, _)| {
                    !r.materialize_writes.is_empty() || !r.held.is_empty() || !r.events.is_empty()
                })
                .map(|(_, n)| n.address.as_str())
                .collect();
            assert_eq!(delivered, [want], "{addr}: the two paths disagree");
        }
        // An address no node claims resolves nowhere on either path.
        assert!(plan
            .osc_in_message("/nowhere/time", &[Arg::F32(0.25)])
            .is_none());
    }
}
