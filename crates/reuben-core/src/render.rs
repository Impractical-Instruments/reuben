//! Render — executing a [`Plan`] per block.
//!
//! The serial executor walks the topologically-ordered nodes. For each node, incoming Messages are
//! routed to its input ports by the port's [`PortKind`]: a **Value** control (scalar / enum /
//! `Harmony`) drives **block-slicing** (the block is split at its change frames so
//! [`Operator::process`](crate::operator::Operator::process) always sees a constant held value,
//! read via `io.read` on a `Held<T>` handle); an **Event** (`Note`) is delivered as a zero-copy
//! [`Event`] on its port (read via an `Event<Note>` handle); a **Signal**
//! [`Buffer`](crate::descriptor::PortType::F32Buffer) input fed by a scalar is **materialized** ZOH
//! into its arena buffer (read via a `SignalF32` handle).
//!
//! Polyphony is hosted inside the Voicer (N voice sub-plans summed), not fanned out across the
//! engine — the retired Lane model.
//!
//! **Realtime-safe.** A [`Renderer`] preallocates its edge-buffer arena and every piece
//! of per-block scratch at construction, and reuses them; steady-state [`Renderer::render_block`]
//! performs no heap allocation (verified by `tests/rt_safe.rs`). Per-port buffer wiring
//! uses stack [`SmallVec`]s; routed events are zero-copy views onto the caller's Messages.
//!
//! The [`Executor`] trait is the pluggable-executor seam.

use alloc::vec;
use alloc::vec::Vec;
use num_traits::Float;

use smallvec::SmallVec;

use crate::coordinator::retire::{retire_to, RetireQueue};
use crate::descriptor::{Port, PortType};
use crate::message::{Arg, Emit, Event, Message};
use crate::meter::Block;
use crate::operator::Io;
use crate::plan::{InterfaceOutput, Plan, PlanNode, PortKind};
use crate::signal::{AudioSample, BlockMut};

/// Decides the order in which nodes are processed for a block.
///
/// The plan is already topologically ordered, so a valid execution is simply `0..nodes.len()`. A
/// future parallel executor returns the same set grouped into concurrently-runnable clusters. The
/// order is written into a caller-owned buffer (reused across blocks) so producing it allocates
/// nothing in steady state.
///
/// **An implementation must keep each node at its planned index.** Instantiate's arena liveness
/// pass hands one edge buffer to several producers, dated by *that* linearization — so two
/// independent branches can share a slot, and running one where the other was planned makes them
/// overwrite each other. Grouping consecutive indices into concurrent clusters is not the same
/// freedom as reordering them, and a genuinely order-free executor needs the liveness question
/// re-asked, not just this trait re-implemented.
pub trait Executor {
    fn order(&self, plan: &Plan, out: &mut Vec<usize>);
}

/// Single-threaded executor: process nodes in topo order. (MVP.)
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

/// Reusable per-block render scratch: everything the render path needs
/// *besides* the edge-buffer arena. Preallocated once (sized to a plan) and reused, so the render
/// path is allocation-free in steady state. Kept separate from the arena so [`render_plan`] is a
/// re-entrant free function — a hosting operator (`Voicer`) renders each sub-plan with that
/// sub-plan's own arena while reusing one shared `RenderScratch`.
pub struct RenderScratch {
    /// Per-node scratch: the current node's signal-output buffers, swapped out of `arena` so
    /// inputs (still in `arena`) and outputs are disjointly borrowable. In signal-output port
    /// order. Cleared and refilled per node — capacity retained.
    out_scratch: Vec<Vec<AudioSample>>,
    /// Per-block message routing, one entry per node. Reused; inner Vecs cleared per block.
    routes: Vec<NodeRoute>,
    /// Block-lifetime pool of operator-emitted Messages. Routed events borrow
    /// from it, so it is grown once and only cleared (never freed) per block.
    emitted: Vec<Emit>,
    /// One node's emissions for the current node, drained into `emitted` after it runs.
    emit_scratch: Vec<Emit>,
    /// Per-node block-slice boundaries scratch. Cleared per node — capacity retained.
    bounds: Vec<usize>,
    /// Execution order for the block, refilled by the executor into reused capacity.
    order: Vec<usize>,
    /// One node segment's Stream events, grouped by input port: port `p`'s events are
    /// `seg_events[seg_starts[p]..seg_starts[p + 1]]`. Held between blocks only while empty (see
    /// [`recycle_events`]), so the capacity a burst grows it to is kept rather than rebuilt per
    /// segment.
    seg_events: Vec<Event<'static>>,
    /// Port boundaries into `seg_events`, `n_inputs + 1` long for the node being stepped.
    seg_starts: Vec<usize>,
}

impl RenderScratch {
    /// Preallocate scratch sized to `plan` so the steady-state render path never grows it.
    pub fn new(plan: &Plan) -> Self {
        let routes = plan.nodes().iter().map(NodeRoute::presized).collect();
        let max_out_bufs = plan
            .nodes()
            .iter()
            .map(|n| n.outputs.len())
            .max()
            .unwrap_or(0);
        let max_inputs = plan
            .nodes()
            .iter()
            .map(|n| n.descriptor.inputs.len())
            .max()
            .unwrap_or(0);
        Self {
            out_scratch: Vec::with_capacity(max_out_bufs),
            routes,
            emitted: Vec::with_capacity(EMIT_POOL_CAP),
            emit_scratch: Vec::with_capacity(EMIT_POOL_CAP),
            bounds: Vec::with_capacity(8),
            order: Vec::with_capacity(plan.nodes.len()),
            seg_events: Vec::with_capacity(EMIT_POOL_CAP),
            seg_starts: Vec::with_capacity(max_inputs + 1),
        }
    }
}

/// Owns the edge-buffer arena and drives Render for a single Plan.
///
/// All buffers and per-block scratch are allocated once here and reused, so
/// [`Renderer::render_block`] is allocation-free in steady state.
pub struct Renderer<E: Executor = SerialExecutor> {
    /// Edge buffers, indexed by arena slot; one block long each.
    arena: Vec<Vec<AudioSample>>,
    /// Reusable per-block scratch (everything but the arena), shared across re-entrant renders.
    scratch: RenderScratch,
    /// Throwaway outbound sink for the mono [`Renderer::render_block`] convenience, which has no
    /// outbound out-parameter. Preallocated and cleared per call so render_block stays alloc-free.
    outbound_sink: Vec<Message>,
    /// Per-channel master scratch, one buffer per logical channel, each
    /// `block_size` long. Used by the mono [`Renderer::render_block`] convenience so it can
    /// compute the full N-channel master and hand back channel 0; preallocated and reused.
    master: Vec<Vec<AudioSample>>,
    /// The pluggable executor. Decides node order each block.
    executor: E,
    block_size: usize,
    /// Where a displaced heap-owning value goes instead of being dropped on the render thread.
    /// `None` until a Coordinator attaches one; values are then dropped where they are replaced.
    retire: Option<RetireQueue>,
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
            retire: None,
        }
    }

    /// Retire the values this renderer displaces into `queue`. Off the render thread: it drops the
    /// handle it replaces.
    pub fn set_retire_queue(&mut self, queue: RetireQueue) {
        self.retire = Some(queue);
    }

    /// The queue attached with [`set_retire_queue`](Self::set_retire_queue), if any.
    pub fn retire_queue(&self) -> Option<&RetireQueue> {
        self.retire.as_ref()
    }

    /// Render one block into a mono buffer — the historical convenience, kept for tests,
    /// examples, and single-channel callers. `out` is `block_size` long and receives **logical
    /// channel 0** of the master. For a broadcast/mono instrument every channel is identical,
    /// so this is bit-identical to the pre-stereo output; for a true stereo patch it is the
    /// left channel only (use [`Renderer::render_block_multi`] for both). Supplies **no input
    /// master**: a channel-bound input pipe reads zeros here (`render_block_multi` takes the
    /// input buffers). Allocation-free.
    pub fn render_block(&mut self, plan: &mut Plan, messages: &[Message], out: BlockMut<'_>) {
        debug_assert_eq!(out.len(), self.block_size);
        // Borrow `master`/`outbound_sink` out of `self` so `render_into` can take `&mut self` (both
        // preallocated; `take` swaps in an empty Vec, no allocation). The mono path discards
        // outbound — it has no out-parameter — so it renders into a throwaway sink.
        let mut master = core::mem::take(&mut self.master);
        let mut outbound = core::mem::take(&mut self.outbound_sink);
        outbound.clear();
        self.render_into(
            plan,
            Inbound::Addressed(messages),
            &[],
            &mut master,
            &mut outbound,
        );
        if let Some(ch0) = master.first() {
            out.copy_from_slice(&ch0[..out.len()]);
        } else {
            out.iter_mut().for_each(|s| *s = 0.0);
        }
        self.master = master;
        self.outbound_sink = outbound;
    }

    /// Render one block across **N logical master channels**. `out` has one buffer
    /// per channel (`out.len() == plan.config.channels`), each `block_size` long. This is the
    /// stereo/multichannel path the engine drives.
    ///
    /// `inputs` is the **logical input master**, the dual of `out`: one buffer
    /// per logical input channel (`inputs.len() == plan.config.input_channels`), each at least
    /// `block_size` long, that this block's channel-bound input pipes read. It is also the
    /// determinism seam: the offline/test path injects known buffers here, so a
    /// render with injected input is bit-reproducible. A channel the caller does not supply
    /// (`inputs` shorter than the bound width) leaves its pipes on the ordinary ZOH path —
    /// the pipe's **declared default materializes** (a bare pipe's zero seed fills silence)
    /// and routed messages still drive it (dark-degrade); a supplied-but-short
    /// buffer's tail reads **zeros**. A no-input patch passes `&[]` and pays nothing.
    ///
    /// `outbound` receives any Messages an `osc_out` sink sent this block; it is
    /// **appended to, never cleared** — the caller drains it.
    /// Allocation-free in steady state.
    pub fn render_block_multi(
        &mut self,
        plan: &mut Plan,
        messages: &[Message],
        inputs: &[Vec<AudioSample>],
        out: &mut [Vec<AudioSample>],
        outbound: &mut Vec<Message>,
    ) {
        self.render_into(plan, Inbound::Addressed(messages), inputs, out, outbound);
    }

    /// [`Renderer::render_block_multi`] over control already resolved against `plan`.
    pub(crate) fn render_block_resolved(
        &mut self,
        plan: &mut Plan,
        messages: &[Resolved],
        inputs: &[Vec<AudioSample>],
        out: &mut [Vec<AudioSample>],
        outbound: &mut Vec<Message>,
    ) {
        self.render_into(plan, Inbound::Resolved(messages), inputs, out, outbound);
    }

    /// The shared render path: execute every node for the block and sum the master taps into
    /// `master` (one buffer per logical channel). `master.len()` should equal
    /// `plan.config.channels`; a tap addressing a channel beyond `master` is dropped. Delegates to
    /// the re-entrant [`render_plan`] over this renderer's owned arena + scratch.
    fn render_into(
        &mut self,
        plan: &mut Plan,
        messages: Inbound<'_>,
        inputs: &[Vec<AudioSample>],
        master: &mut [Vec<AudioSample>],
        outbound: &mut Vec<Message>,
    ) {
        render_inbound(
            plan,
            &mut self.arena,
            &mut self.scratch,
            &self.executor,
            messages,
            self.block_size,
            inputs,
            master,
            outbound,
            self.retire.as_ref(),
        );
    }
}

/// Re-entrant block render over an explicit `(plan, arena, scratch)`: feed the
/// input master from `inputs` (one buffer per logical input channel; an
/// unsupplied channel falls back to its pipe's declared default — a no-input plan passes
/// `&[]`), execute every node for
/// `frames` frames, and sum the master taps into `master` (one buffer per logical channel,
/// `master.len()` should equal `plan.config.channels`; a tap beyond `master` is dropped). `outbound`
/// is **appended to** — the caller drains it.
///
/// This is the primitive that makes render re-entrant: render is a pure function of
/// `(plan, arena, scratch)`, not nested mutable renderer state. The top-level [`Renderer`] calls it
/// with the rig's arena; a hosting operator (`Voicer`) calls the *same* function per
/// active voice with that voice's own arena, reusing one shared `RenderScratch`. Allocation-free in
/// steady state when `scratch` was [`RenderScratch::new`]-sized to `plan`. A value the render
/// displaces goes to `retire`, which a hosting operator passes on from its own [`Io`].
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
    arena: &mut [Vec<AudioSample>],
    scratch: &mut RenderScratch,
    executor: &E,
    messages: &[Message],
    frames: usize,
    inputs: &[Vec<AudioSample>],
    master: &mut [Vec<AudioSample>],
    outbound: &mut Vec<Message>,
    retire: Option<&RetireQueue>,
) {
    render_inbound(
        plan,
        arena,
        scratch,
        executor,
        Inbound::Addressed(messages),
        frames,
        inputs,
        master,
        outbound,
        retire,
    );
}

#[allow(clippy::too_many_arguments)]
fn render_inbound<E: Executor>(
    plan: &mut Plan,
    arena: &mut [Vec<AudioSample>],
    scratch: &mut RenderScratch,
    executor: &E,
    messages: Inbound<'_>,
    frames: usize,
    inputs: &[Vec<AudioSample>],
    master: &mut [Vec<AudioSample>],
    outbound: &mut Vec<Message>,
    retire: Option<&RetireQueue>,
) {
    // Fresh edge buffers each block (upstream writes before downstream reads). Materialize
    // scratch buffers are excluded: they are fully written by the materialize step
    // and persist a held input's value across blocks, so zeroing them would only force a
    // needless refill. `process_node` keeps each one fully defined (see `materialize_clean`).
    for (i, buf) in arena.iter_mut().enumerate() {
        if plan.materialize_scratch_mask[i] {
            continue;
        }
        buf.iter_mut().for_each(|s| *s = 0.0);
    }

    // Feed the input master: copy each caller-supplied logical input channel
    // into its bound pipe's `in` materialize scratch before any node runs, and flag the entry
    // `materialize_device_fed` so `process_node`'s ZOH fill yields the buffer for the block
    // (device audio wins). Distinct taps may share a channel (fan-out at the master). A
    // channel the caller doesn't supply is left alone: the ordinary materialize fill then
    // renders the pipe's declared default and applies routed messages — dark-degrade
    // to the control the pipe advertises, not to a dead knob. The tail of a
    // supplied-but-short buffer reads zeros. No-input plans have no taps and pay nothing.
    for i in 0..plan.input_taps.len() {
        let tap = plan.input_taps[i];
        let Some(src) = inputs.get(tap.channel) else {
            continue;
        };
        let dst = &mut arena[tap.buffer];
        let supplied = frames.min(src.len());
        dst[..supplied].copy_from_slice(&src[..supplied]);
        dst[supplied..frames].fill(0.0);
        plan.nodes[tap.node].materialize_device_fed[tap.mat_index] = true;
    }

    // Route external messages to node input ports, classified by port kind. Reuses scratch.
    // Operator-emitted messages are routed later, interleaved with execution.
    route_messages(&mut scratch.routes, plan, messages, retire);
    // Last block's emissions: a forwarded string here may outlive every latch it was copied into.
    for e in scratch.emitted.drain(..) {
        retire_to(retire, e.arg);
    }

    // Executor decides node order for the block (into reused capacity).
    executor.order(plan, &mut scratch.order);

    let sample_rate = plan.config.sample_rate;
    let block_size = frames;
    let probing = plan.probes.any_on();
    // Strings an upstream forward carried that a `str` input refused, added to the Plan's count
    // once the node loop's borrows end.
    let mut unprepared = 0u64;

    // Disjoint field borrows so `process_node` can hold `arena` (inputs) and
    // `out_scratch` (outputs) at once while reading `routes`/`order`.
    let RenderScratch {
        out_scratch,
        routes,
        emitted,
        emit_scratch,
        bounds,
        order,
        seg_events,
        seg_starts,
    } = scratch;

    for &i in order.iter() {
        emit_scratch.clear();
        process_node(
            arena,
            out_scratch,
            bounds,
            seg_events,
            seg_starts,
            &mut routes[i],
            &mut plan.nodes[i],
            messages,
            emitted,
            emit_scratch,
            sample_rate,
            block_size,
            retire,
        );

        // Measured now or never: once a later node runs, this node's output slots may be
        // recycled to it.
        if let Some(p) = plan.nodes[i].probe.filter(|_| probing) {
            if plan.probes.nodes.get(p).is_some_and(|n| n.is_on()) {
                for (port, &buf) in plan.nodes[i].outputs.iter().enumerate() {
                    let block = Block::measure(arena[buf][..frames].iter().copied());
                    plan.probes.accumulate(p, port, block);
                }
            }
        }

        // An `osc_out` sink: its emissions leave the graph. Drain each to
        // the outbound list stamped with the node's (fixed) address and the already-block-
        // absolute frame; native encodes + sends them. A sink has no downstream wiring, so this
        // replaces — not supplements — the routing below.
        if let Some(addr) = plan
            .outbound_taps
            .iter()
            .find(|t| t.node == i)
            .map(|t| &*t.address)
        {
            for e in emit_scratch.drain(..) {
                outbound.push(Message::new(addr, e.arg, e.frame));
            }
            continue;
        }

        // Route this node's emissions: each goes into the block-lifetime
        // pool, and is delivered to every wired `(dst node, dst input port)` — which run later
        // in topo order, so they see it. The dst input port's [`PortKind`] decides how it
        // lands: a Value input latches + re-slices (the former context publish unifies here), a
        // Event input gets a zero-copy event, a Signal input materializes ZOH.
        for e in emit_scratch.drain(..) {
            let port = e.port;
            let pool_idx = emitted.len();
            // Capture a Value `interface` output: its last-emitted scalar is held for
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
                            routes[dst].push_materialize(e.frame, dst_port, v);
                        }
                    }
                    PortKind::Value => {
                        let p = &plan.nodes[dst].descriptor.inputs[dst_port];
                        match held_arg(p, &e.arg) {
                            Some(a) => routes[dst].held.push((e.frame, dst_port, a)),
                            None => unprepared += u64::from(is_unprepared(p, &e.arg)),
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
    plan.dropped_unprepared += unprepared;

    // Sum master taps into the per-channel master: every tapped port's buffer,
    // in fixed order, so output stays deterministic. A broadcast tap
    // (`channel: None`) adds to every channel — the historical mono fan, so channel 0 of a
    // fully-broadcast instrument is bit-identical to the pre-stereo single buffer. A
    // channel-pinned tap adds to that one channel only.
    for chan in master.iter_mut() {
        chan.iter_mut().for_each(|s| *s = 0.0);
    }
    for tap in &plan.output_taps {
        let Some(buf) = tap.buffer else { continue };
        match tap.channel {
            None => {
                for chan in master.iter_mut() {
                    for (o, s) in chan.iter_mut().zip(arena[buf].iter()) {
                        *o += *s;
                    }
                }
            }
            Some(c) => {
                if let Some(chan) = master.get_mut(c) {
                    for (o, s) in chan.iter_mut().zip(arena[buf].iter()) {
                        *o += *s;
                    }
                }
            }
        }
    }

    if probing && !plan.hosted {
        plan.probes.commit(frames);
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
        let messages = Inbound::Addressed(messages);
        let retire = self.retire.as_ref();
        route_messages(&mut self.scratch.routes, plan, messages, retire);
        for e in self.scratch.emitted.drain(..) {
            retire_to(retire, e.arg);
        }
        let sample_rate = plan.config.sample_rate;

        let arena = &mut self.arena;
        let RenderScratch {
            out_scratch,
            routes,
            emitted,
            emit_scratch,
            bounds,
            seg_events,
            seg_starts,
            ..
        } = &mut self.scratch;
        emit_scratch.clear();
        process_node(
            arena,
            out_scratch,
            bounds,
            seg_events,
            seg_starts,
            &mut routes[node_idx],
            &mut plan.nodes[node_idx],
            messages,
            emitted,
            emit_scratch,
            sample_rate,
            frames,
            retire,
        );
    }

    /// Read edge-arena buffer `bi` (a node output, or a driven input's scratch). Length `block_size`.
    pub(crate) fn arena_buffer(&self, bi: usize) -> crate::signal::BlockView<'_> {
        &self.arena[bi]
    }

    /// Mutable edge-arena buffer `bi`, for writing a driven (time-varying audio-in) buffer before a
    /// [`Renderer::step_node`] call. The slot must be materialize scratch so the per-block clear skips it.
    pub(crate) fn arena_buffer_mut(&mut self, bi: usize) -> BlockMut<'_> {
        &mut self.arena[bi]
    }

    /// The emissions the last [`Renderer::step_node`] collected (segment-relative frames).
    pub(crate) fn last_emits(&self) -> &[Emit] {
        &self.scratch.emit_scratch
    }
}

/// Normalize a routed [`Arg`] for a **Held** input port: clamp an `F32` to the port's
/// range, resolve an enum control message (symbol / index / variant) to the enum's concrete `Arg`,
/// take on a `str` port only a value its preparer built, or take any other vocab value
/// (`Harmony`) as-is. `None` if it cannot be a value of this port.
pub(crate) fn held_arg(p: &Port, arg: &Arg) -> Option<Arg> {
    match &p.ty {
        PortType::F32 => arg
            .as_f32()
            .map(|v| Arg::F32(p.meta.as_ref().map(|m| m.clamp(v)).unwrap_or(v))),
        // A held integer control: a runtime message quantizes exactly as an author
        // literal does through [`Port::coerce`] — an `Arg::I32` clamps directly (through f32 it
        // would lose every integer past 2^24); any other number rounds to the nearest integer,
        // then clamps to the declared range. So an `i32` pipe driven `/steps/in 12.7` latches `13`,
        // not `12.7`; the "int control" promise holds for live OSC input, not only authored
        // defaults. An authored JSON literal reaches `coerce` already narrowed to f32, so one past
        // 2^24 has lost precision before either path sees it.
        PortType::I32 { meta } => {
            let i = match arg {
                Arg::I32(i) => *i,
                _ => Float::round(arg.as_f32()?) as i32,
            };
            Some(Arg::I32(meta.as_ref().map(|m| m.clamp(i)).unwrap_or(i)))
        }
        PortType::Vocab {
            enum_meta: Some(e), ..
        } => e.resolve_arg(arg),
        // A refcount bump. Raw text and another preparer's value are refused alike: the input
        // reads its own preparer's payload, and a live message names its port by address alone.
        PortType::Str { preparer } => match arg {
            Arg::Prepared(v) if preparer.is_some_and(|q| q.kind == v.kind()) => Some(arg.clone()),
            _ => None,
        },
        _ => Some(arg.clone()),
    }
}

/// Whether `held_arg` refusing `arg` at `p` is a string the input could not take: raw text, or a
/// value another preparer built. Counted, unlike a number sent to a `str` input.
fn is_unprepared(p: &Port, arg: &Arg) -> bool {
    matches!(p.ty, PortType::Str { .. }) && matches!(arg, Arg::Str(_) | Arg::Prepared(_))
}

/// Where a routed event's payload lives: an external block-input Message, or a Message an
/// upstream operator emitted this block. Either way delivery is zero-copy — the
/// [`Event`] borrows the source.
#[derive(Clone, Copy)]
enum EventSrc {
    /// Index into the block `messages` slice. The event delivers by the wired input port, so only
    /// the payload (and frame) are carried forward — the external address is not.
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

/// The fewest routed changes a [`NodeRoute`] bucket is presized for: a gesture of a few controls
/// can land several changes on one node in a block, and 4 is what a `Vec`'s first growth would
/// have reserved anyway, so the presize costs no more than the growth it replaces.
const ROUTED_CHANGES_MIN: usize = 4;

/// Per-node routed messages for one block, one bucket per [`PortKind`].
#[derive(Default)]
struct NodeRoute {
    /// (frame, push index, input port, value) — a scalar feeding a materialized
    /// [`Buffer`](PortType::F32Buffer) input. The engine writes it into the input's scratch buffer
    /// at its frame (ZOH); does **not** split the block. Sorted by (frame, push index) before
    /// materialize, so writes to one port at one frame apply in arrival order and the last wins.
    materialize_writes: Vec<(usize, usize, usize, f32)>,
    /// (frame, input port, value) — a change to a **Held** input: scalar / enum /
    /// `Harmony`. Splits the block (a held value is constant per `process` call); each interior
    /// frame becomes a segment boundary and updates the port's latch.
    held: Vec<(usize, usize, Arg)>,
    /// Routed **Stream** events (by reference), one per delivered Message — for event operators.
    events: Vec<RoutedEvent>,
}

impl NodeRoute {
    /// Buckets sized so a Plan's first block allocates nothing: one routed change per input port of
    /// their kind, and never fewer than [`ROUTED_CHANGES_MIN`] for a kind the node has at all. One
    /// per port is what a send or a pipe's forward delivers, and a pipe forwards only on change, so
    /// its first block (each consumer's first held change) is the case this exists for. Nothing
    /// bounds a block's changes to one port — an upstream node may emit at every frame, and a batch
    /// may address one port many times — so past this a bucket grows once on the render thread and
    /// keeps the capacity, the policy `EMIT_POOL_CAP` states for the emit pool.
    fn presized(node: &PlanNode) -> Self {
        let capacity =
            |kind: PortKind| match node.input_kinds.iter().filter(|&&k| k == kind).count() {
                0 => 0,
                n => n.max(ROUTED_CHANGES_MIN),
            };
        Self {
            materialize_writes: Vec::with_capacity(capacity(PortKind::Signal)),
            held: Vec::with_capacity(capacity(PortKind::Value)),
            events: Vec::with_capacity(capacity(PortKind::Event)),
        }
    }

    fn push_materialize(&mut self, frame: usize, port: usize, value: f32) {
        let seq = self.materialize_writes.len();
        self.materialize_writes.push((frame, seq, port, value));
    }
}

/// Where one inbound control message lands in a [`Plan`]: a node's input port, or a dissolved
/// interface pipe that delivers to its rewired consumer. An index into the Plan it was resolved
/// against, so it means nothing to any other Plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    /// Index into [`Plan::input_aliases`].
    Alias(usize),
    /// Node index in execution order, and that node's input port index.
    Port { node: usize, port: usize },
}

/// An inbound control message already resolved to its [`Target`] — the form the [`Engine`]
/// queues, so delivering it neither carries nor matches an address.
///
/// [`Engine`]: crate::engine::Engine
#[derive(Debug)]
pub(crate) struct Resolved {
    pub(crate) target: Target,
    pub(crate) frame: usize,
    pub(crate) arg: Arg,
}

/// One block's inbound control: addressed [`Message`]s, resolved at routing time, or control the
/// caller already resolved against this Plan.
#[derive(Clone, Copy)]
enum Inbound<'a> {
    Addressed(&'a [Message]),
    Resolved(&'a [Resolved]),
}

impl<'a> Inbound<'a> {
    fn len(self) -> usize {
        match self {
            Inbound::Addressed(m) => m.len(),
            Inbound::Resolved(m) => m.len(),
        }
    }

    /// Message `i`'s payload and block frame.
    fn get(self, i: usize) -> (&'a Arg, usize) {
        match self {
            Inbound::Addressed(m) => (&m[i].arg, m[i].frame),
            Inbound::Resolved(m) => (&m[i].arg, m[i].frame),
        }
    }

    /// Message `i`'s target, payload and block frame; `None` if its address reaches nothing.
    fn resolve(self, plan: &Plan, i: usize) -> Option<(Target, &'a Arg, usize)> {
        match self {
            Inbound::Addressed(m) => {
                let msg = &m[i];
                let (target, _) = plan.resolve(&msg.address)?;
                Some((target, &msg.arg, msg.frame))
            }
            Inbound::Resolved(m) => Some((m[i].target, &m[i].arg, m[i].frame)),
        }
    }
}

/// Deliver each message to its [`Target`], classified by the destination port's [`PortKind`].
/// Reuses `routes` (one entry per node); clears and refills.
fn route_messages(
    routes: &mut Vec<NodeRoute>,
    plan: &mut Plan,
    messages: Inbound<'_>,
    retire: Option<&RetireQueue>,
) {
    // Steady-state no-op: the Renderer presizes `routes` to the node count.
    if routes.len() != plan.nodes.len() {
        routes.resize_with(plan.nodes.len(), NodeRoute::default);
    }
    for r in routes.iter_mut() {
        r.materialize_writes.clear();
        // `process_node` moves each applied value into its latch, so what is left is a placeholder
        // or a change framed past the block, which was never applied.
        for (_, _, arg) in r.held.drain(..) {
            retire_to(retire, arg);
        }
        r.events.clear();
    }

    if messages.len() > 0 {
        for a in plan.input_aliases.iter_mut() {
            a.last_frame = None;
        }
    }

    for mi in 0..messages.len() {
        let Some((target, arg, frame)) = messages.resolve(plan, mi) else {
            continue;
        };
        match target {
            // A dissolved interface pipe: deliver to its rewired consumer with the pipe's own
            // normalization — the same two hops (pipe port, then consumer port) the rendered pipe
            // node made.
            Target::Alias(ai) => {
                let alias = &plan.input_aliases[ai];
                let (n, port) = (alias.node, alias.dst_port);
                // A fed pipe's value is its feeder's.
                if alias.seed.is_none() {
                    plan.dropped_wired_writes += 1;
                    continue;
                }
                // What the rendered pipe would now hold, for a numeric pipe.
                let mut held = None;
                match alias.kind {
                    // A signal pipe's message feed is a ZOH materialize write, clamped to the
                    // pipe's range as the OSC boundary clamps it, so a typed Message lands the
                    // same.
                    PortKind::Signal => {
                        if let Some(v) = arg.as_f32().map(|v| clamp_to(&alias.port, v)) {
                            routes[n].push_materialize(frame, port, v);
                            held = Some(Arg::F32(v));
                        }
                    }
                    // A value pipe latched (pipe-meta clamp), then forwarded; the emission landed
                    // by the consumer port's kind — Value re-`held_arg`s, Signal materializes.
                    PortKind::Value => {
                        let Some(a) = held_arg(&alias.port, arg) else {
                            if is_unprepared(&alias.port, arg) {
                                plan.dropped_unprepared += 1;
                            }
                            continue;
                        };
                        match plan.nodes[n].input_kinds[port] {
                            PortKind::Signal => {
                                if let Some(v) = a.as_f32() {
                                    routes[n].push_materialize(frame, port, v);
                                }
                            }
                            PortKind::Value => {
                                let p = &plan.nodes[n].descriptor.inputs[port];
                                match held_arg(p, &a) {
                                    Some(a2) => routes[n].held.push((frame, port, a2)),
                                    None if is_unprepared(p, &a) => plan.dropped_unprepared += 1,
                                    None => {}
                                }
                            }
                            // Never aliased: dissolve keeps a Value pipe feeding an Event input.
                            PortKind::Event => {}
                        }
                        if matches!(a, Arg::F32(_) | Arg::I32(_)) {
                            held = Some(a);
                        }
                    }
                    // An event pipe re-emitted each routed event verbatim at its frame.
                    PortKind::Event => {
                        routes[n].events.push(RoutedEvent {
                            dst_port: port,
                            src: EventSrc::External { msg: mi },
                        });
                    }
                }
                let alias = &mut plan.input_aliases[ai];
                if let Some(held) = held {
                    if alias.last_frame.is_none_or(|f| frame >= f) {
                        alias.last = held;
                        alias.last_frame = Some(frame);
                    }
                }
            }
            // Routing is by port, not by operator address-filtering.
            Target::Port { node: i, port } => {
                // Writing a fed input is unsupported: its value is its feeder's.
                if plan.nodes[i].wired[port] {
                    plan.dropped_wired_writes += 1;
                    continue;
                }
                match plan.nodes[i].input_kinds[port] {
                    // Clamped as the OSC boundary clamps it, so a typed Message lands the same.
                    PortKind::Signal => {
                        let p = &plan.nodes[i].descriptor.inputs[port];
                        if let Some(v) = arg.as_f32().map(|v| clamp_to(p, v)) {
                            routes[i].push_materialize(frame, port, v);
                        }
                    }
                    PortKind::Value => {
                        let p = &plan.nodes[i].descriptor.inputs[port];
                        match held_arg(p, arg) {
                            Some(a) => routes[i].held.push((frame, port, a)),
                            None if is_unprepared(p, arg) => plan.dropped_unprepared += 1,
                            None => {}
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
        }
    }
}

/// `v` clamped to `p`'s declared range, if it declares one.
fn clamp_to(p: &Port, v: f32) -> f32 {
    p.meta.as_ref().map(|m| m.clamp(v)).unwrap_or(v)
}

/// Resolve an inbound address to its destination — `(node index, input port index, port)` —
/// by matching a node address prefix ([`local_address`]) and then an input port by name. The
/// **single** "address → node + port" rule, reached only through [`Plan::resolve`], so routing
/// an addressed [`Message`] and queueing inbound OSC cannot diverge — a diverged copy would
/// silently drop messages to nested nodes.
///
/// A node whose address prefix-matches but has **no** matching port does not decide the
/// outcome — keep scanning. Node addresses may be ancestors of one another (`/fx` beside an
/// inlined `/fx/verb/delay`, inlining manufactures these systematically), and
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
/// `out_scratch`, and events are zero-copy views grouped in the reusable `seg_events`.
#[allow(clippy::too_many_arguments)]
fn process_node(
    arena: &mut [Vec<AudioSample>],
    out_scratch: &mut Vec<Vec<AudioSample>>,
    bounds: &mut Vec<usize>,
    seg_events_slot: &mut Vec<Event<'static>>,
    seg_starts: &mut Vec<usize>,
    route: &mut NodeRoute,
    node: &mut PlanNode,
    messages: Inbound<'_>,
    emitted: &[Emit],
    emit_scratch: &mut Vec<Emit>,
    sample_rate: f32,
    block_size: usize,
    retire: Option<&RetireQueue>,
) {
    // The ZOH fill needs the writes in frame order. Not `sort_by_key`: a stable sort allocates its
    // merge buffer once a burst outgrows the stack scratch; the push index is the tie-break that
    // keeps arrival order instead.
    route
        .materialize_writes
        .sort_unstable_by_key(|&(f, seq, _, _)| (f, seq));

    // Materialize each Buffer-from-scalar input into its scratch buffer: fill from the
    // latched scalar, overwrite with each mid-block change from its frame onward, persist the final
    // value as the next block's latch, and flag `varying`. Unwired inputs materialize too — a bare
    // buffer's latch seeds 0.0, so it fills with silence — which is what upholds the
    // buffer-presence invariant: every Signal input reaches `process` as a dense
    // length-n slice. Done before the output-swap — scratch buffers are inputs, disjoint from this
    // node's outputs. These writes do NOT split the block; sample-accuracy comes from writing them
    // into the buffer at their frame.
    //
    // Cached steady state: a held-unchanged input is refilled only when it must be. Its scratch is
    // excluded from the per-block arena clear, so it persists; a constant block leaves it untouched.
    for k in 0..node.materialize.len() {
        let (port, buf) = node.materialize[k];
        // The input master already wrote device audio into this scratch for the block
        // (`InputTap`): device audio wins — skip the ZOH fill (and this block's
        // routed messages; the latch keeps its value for the next unsupplied block). The
        // buffer no longer holds the latch, so mark it dirty for the next ZOH refill, and
        // `varying` — device audio is per-sample data, not a held constant.
        if node.materialize_device_fed[k] {
            node.materialize_device_fed[k] = false;
            node.materialize_clean[k] = false;
            node.varying[port] = true;
            continue;
        }
        let target = &mut arena[buf];
        let changed = route
            .materialize_writes
            .iter()
            .any(|&(_, _, p, _)| p == port);
        // The latch is the sole ZOH store: a materialized port is always seeded/set
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
        for &(f, _, p, val) in &route.materialize_writes {
            if p != port {
                continue;
            }
            let f = f.min(block_size);
            target[cursor..f].fill(v);
            cursor = f;
            v = val;
        }
        target[cursor..block_size].fill(v);
        // Persist the end-of-block value as the next block's ZOH: `latch` is the single
        // source of truth — the materialized buffer is the sample-accurate path, the latch what a
        // held-handle `io.read` sees. A Buffer port carries no meaningful held value, so the write is ignored there.
        node.latch[port] = Arg::F32(v);
        node.varying[port] = true;
        node.materialize_clean[k] = false;
    }

    // Segment boundaries: 0, block_size, every interior Held-change frame (a held value is constant
    // per `process` call). Sort + dedup since change frames interleave.
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

    // Re-zero the output slots an earlier producer in this block already wrote. The per-block
    // edge clear ran before any node did, so it left the block's *first* producer of each
    // recycled slot a fresh buffer and no one else; this restores that for the rest. Empty
    // unless Instantiate's liveness pass recycled a slot into this node.
    for &bi in &node.recycled_outputs {
        arena[bi].fill(0.0);
    }

    // Swap this node's signal-output buffers out of the arena into `out_scratch` (disjoint from
    // inputs — no self-loops; cycles error), in signal-output port order.
    out_scratch.clear();
    for &bi in &node.outputs {
        out_scratch.push(core::mem::take(&mut arena[bi]));
    }
    let n_inputs = node.descriptor.inputs.len();

    // A segment's events are a subset of this node's routed events, so after this reserve the
    // segment loop never grows `seg_events`. It allocates only when a block routes more events to
    // one node than any block before it did, and the grown capacity is kept.
    let mut seg_events = recycle_events(core::mem::take(seg_events_slot));
    seg_events.reserve(route.events.len());

    for w in bounds.windows(2) {
        let (seg_start, seg_end) = (w[0], w[1]);
        if seg_start >= seg_end {
            continue;
        }

        // Apply Held changes landing at this segment's start: update the per-port latch, read via
        // `io.read` on a `Held<T>` handle. Persists across blocks (the latch is next block's frame-0 baseline).
        // Last-write-wins on equal frames. Each value is moved out of `route.held`, so the routing
        // scratch holds no second reference, and the value it displaces is retired.
        for (f, port, arg) in route.held.iter_mut() {
            if *f == seg_start {
                let new = core::mem::replace(arg, Arg::F32(0.0));
                retire_to(retire, core::mem::replace(&mut node.latch[*port], new));
            }
        }

        // Per-input-port Stream events whose frame falls in this segment, as zero-copy views with
        // segment-relative frames, grouped by port with a counting sort. Filling in `route.events`
        // order keeps each port's events in delivery order.
        seg_events.clear();
        seg_starts.clear();
        seg_starts.resize(n_inputs + 1, 0);
        let in_segment = |re: &RoutedEvent| -> Option<Event<'_>> {
            let (arg, frame): (&Arg, usize) = match re.src {
                EventSrc::External { msg } => messages.get(msg),
                EventSrc::Emitted(idx) => {
                    let e = &emitted[idx];
                    (&e.arg, e.frame)
                }
            };
            (frame >= seg_start && frame < seg_end).then(|| Event {
                arg,
                frame: frame - seg_start,
            })
        };
        // Count into `seg_starts[p + 1]`, then turn the counts into each port's start, held one
        // slot to the right so the fill below can use it as port `p`'s write cursor. After the
        // fill, `seg_starts[p + 1]` has advanced to port `p`'s end, which is port `p + 1`'s start.
        let mut first = None;
        for re in route.events.iter() {
            if let Some(ev) = in_segment(re) {
                first.get_or_insert(ev);
                seg_starts[re.dst_port + 1] += 1;
            }
        }
        if let Some(first) = first {
            let mut start = 0;
            for slot in &mut seg_starts[1..] {
                let count = *slot;
                *slot = start;
                start += count;
            }
            // Every slot is overwritten by the fill; `first` is only a valid placeholder value.
            seg_events.resize(start, first);
            for re in route.events.iter() {
                if let Some(ev) = in_segment(re) {
                    let cursor = &mut seg_starts[re.dst_port + 1];
                    seg_events[*cursor] = ev;
                    *cursor += 1;
                }
            }
        }
        let stream_refs: SmallVec<[&[Event]; 24]> = seg_starts
            .windows(2)
            .map(|w| &seg_events[w[0]..w[1]])
            .collect();

        let inputs = node
            .inputs
            .iter()
            .map(|src| src.map(|bi| &arena[bi][seg_start..seg_end]));

        // Output slices for this segment, in signal-output port order.
        let outputs = out_scratch
            .iter_mut()
            .map(|buf| &mut buf[seg_start..seg_end]);

        // Emitted frames are stamped block-absolute by adding this segment's start.
        let mut io = Io::new(sample_rate, seg_end - seg_start, inputs, outputs)
            .with_latched(&node.latch)
            .with_streams(&stream_refs)
            .with_varying(&node.varying)
            .with_emit(&mut *emit_scratch, seg_start)
            .with_retire(retire);
        node.op.process(&mut io);
    }

    *seg_events_slot = recycle_events(seg_events);

    // Return the signal-output buffers to the arena, same order they were taken.
    for (k, &bi) in node.outputs.iter().enumerate() {
        arena[bi] = core::mem::take(&mut out_scratch[k]);
    }
}

/// Empty `v` and hand its allocation to an `Event` Vec of another lifetime. An [`Event`] borrows
/// this block's Messages and emit pool, so the per-segment event lists cannot outlive one node's
/// step — but their capacity can, as an empty Vec, which carries no borrow.
fn recycle_events<'from, 'to>(mut v: Vec<Event<'from>>) -> Vec<Event<'to>> {
    v.clear();
    let mut v = core::mem::ManuallyDrop::new(v);
    let (ptr, cap) = (v.as_mut_ptr(), v.capacity());
    // SAFETY: `ptr`/`cap` come from a live Vec whose ownership `ManuallyDrop` gives up, so the
    // allocation is freed exactly once, by the returned Vec. `Event<'from>` and `Event<'to>` differ
    // only in lifetime, so size and alignment match the allocation's layout. Length 0: no element
    // is reinterpreted, so no borrow is extended.
    unsafe { Vec::from_raw_parts(ptr.cast::<Event<'to>>(), 0, cap) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AudioConfig;

    /// An `i32` value port quantizes a runtime message — round to nearest, then clamp
    /// to range — exactly as [`crate::descriptor::Port::coerce`] does for an authored literal. So
    /// `/steps/in 12.7` latches `13`, not `12.7`; the "int control" contract holds for live OSC,
    /// not only authored defaults.
    #[test]
    fn held_arg_quantizes_an_int_port() {
        use crate::descriptor::{I32Meta, Port};
        let port = Port::i32(
            "steps",
            I32Meta {
                min: 1,
                max: 16,
                default: 8,
            },
        );
        assert_eq!(held_arg(&port, &Arg::F32(12.7)), Some(Arg::I32(13)));
        assert_eq!(held_arg(&port, &Arg::F32(99.0)), Some(Arg::I32(16))); // clamps to the ceiling
        assert_eq!(held_arg(&port, &Arg::I32(5)), Some(Arg::I32(5))); // integer passes through
    }

    /// An integer arriving at an `i32` port latches exactly across the whole `i32` range, with no
    /// f32 step: 2^24 + 1 is the first integer an f32 cannot hold.
    #[test]
    fn held_arg_keeps_an_int_exact_past_f32_precision() {
        use crate::descriptor::{I32Meta, Port};
        let port = Port::i32(
            "beat",
            I32Meta {
                min: i32::MIN,
                max: i32::MAX,
                default: 0,
            },
        );
        for v in [16_777_217, 16_777_219, i32::MAX - 1, i32::MAX, i32::MIN + 1] {
            assert_eq!(held_arg(&port, &Arg::I32(v)), Some(Arg::I32(v)), "{v}");
        }
        let narrow = Port::i32(
            "steps",
            I32Meta {
                min: 0,
                max: 16_777_216,
                default: 0,
            },
        );
        assert_eq!(
            held_arg(&narrow, &Arg::I32(i32::MAX)),
            Some(Arg::I32(16_777_216)),
            "an int past the range still clamps"
        );
    }

    /// [`held_arg`] and [`Port::coerce`] normalize an `i32` port's value identically, integers past
    /// f32 precision included.
    #[test]
    fn held_arg_and_coerce_agree_on_an_int_port() {
        use crate::descriptor::{I32Meta, Port};
        let port = Port::i32(
            "beat",
            I32Meta {
                min: i32::MIN,
                max: i32::MAX,
                default: 0,
            },
        );
        for arg in [
            Arg::I32(16_777_217),
            Arg::I32(i32::MAX),
            Arg::I32(i32::MIN + 1),
            Arg::I32(-3),
            Arg::F32(12.7),
            Arg::F32(-0.4),
        ] {
            assert_eq!(held_arg(&port, &arg), port.coerce(&arg), "{arg:?}");
        }
        assert_eq!(
            port.coerce(&Arg::I32(16_777_217)),
            Some(Arg::I32(16_777_217))
        );
    }

    /// A `str` port latches only a value its own preparer built, by refcount: the latched value
    /// is the routed allocation. Raw text, another preparer's value and a number are refused, and
    /// a port with no preparer takes nothing.
    #[test]
    fn held_arg_takes_only_its_own_prepared_value_on_a_str_port() {
        use crate::descriptor::Port;
        use crate::prepare::testing::{STEPS, WORDS};
        let port = Port::str_prepared("pat", STEPS);
        let prepared = STEPS.prepare(&"1 2".into()).unwrap();
        let routed = Arg::Prepared(prepared.clone());
        let Some(Arg::Prepared(latched)) = held_arg(&port, &routed) else {
            panic!("its own prepared value latches");
        };
        assert!(latched.ptr_eq(&prepared));

        let other = Arg::Prepared(WORDS.prepare(&"ab".into()).unwrap());
        for refused in [Arg::Str("1 2".into()), other.clone()] {
            assert_eq!(held_arg(&port, &refused), None, "{refused:?}");
            assert!(is_unprepared(&port, &refused), "{refused:?} is counted");
        }
        for refused in [Arg::F32(1.0), Arg::I32(1), Arg::Enum(0)] {
            assert_eq!(held_arg(&port, &refused), None);
            assert!(
                !is_unprepared(&port, &refused),
                "{refused:?} is not a string"
            );
        }
        assert_eq!(held_arg(&Port::str("pat"), &routed), None);
    }

    /// Two leaf nodes in an ancestor-prefix relationship — the shape inlining
    /// manufactures systematically (`/fx` beside an inlined `/fx/verb/delay`). The wire from
    /// `/fx` pins the topo order, so the portless ancestor is genuinely scanned first.
    fn shadowed_plan() -> Plan {
        // Built graph-first rather than loaded: the loader lives in `reuben-document`, above this
        // crate, and both tests below reach `route_messages`, which is private here. The shape is
        // what matters — `/fx` wired into `/fx/verb/delay` pins the topo order so the portless
        // ancestor is genuinely scanned first.
        use crate::graph::Graph;
        use crate::operators::{delay, oscillator, Delay, Oscillator};
        let mut g = Graph::new();
        let fx = g.add("/fx", Oscillator::new());
        let deep = g.add("/fx/verb/delay", Delay::new());
        g.connect(fx, oscillator::OUT_AUDIO, deep, delay::IN_AUDIO);
        g.tap_output(deep, delay::OUT_AUDIO);
        Plan::instantiate(g, AudioConfig::new(48_000.0, 64)).expect("instantiate")
    }

    /// Inbound OSC to a nested node must not be dropped because a shallower
    /// prefix-matching node that lacks the port sits earlier in `plan.nodes`.
    #[test]
    fn osc_in_message_reaches_a_node_shadowed_by_a_portless_ancestor() {
        let plan = shadowed_plan();
        let fx = plan
            .nodes()
            .iter()
            .position(|n| &*n.address == "/fx")
            .expect("/fx node");
        let deep = plan
            .nodes()
            .iter()
            .position(|n| &*n.address == "/fx/verb/delay")
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

    /// Parity guard: the boundary conversion (`osc_in_message`), routing an addressed Message, and
    /// routing control resolved at queue time all agree on the destination node. All three go
    /// through [`Plan::resolve`]; this pins them together behaviorally if any ever re-inlines the
    /// rule.
    #[test]
    fn osc_in_message_and_route_messages_agree_on_the_destination() {
        let mut plan = shadowed_plan();
        let cases = [
            // A direct hit on the ancestor (Signal input: oscillator freq is a signal control).
            ("/fx/freq", "/fx"),
            // Nested targets behind the portless prefix (Value inputs).
            ("/fx/verb/delay/time", "/fx/verb/delay"),
            ("/fx/verb/delay/mix", "/fx/verb/delay"),
        ];
        let mut routes = Vec::new();
        let delivered = |routes: &[NodeRoute], plan: &Plan| -> Vec<alloc::string::String> {
            routes
                .iter()
                .zip(plan.nodes())
                .filter(|(r, _)| {
                    !r.materialize_writes.is_empty() || !r.held.is_empty() || !r.events.is_empty()
                })
                .map(|(_, n)| alloc::string::String::from(&*n.address))
                .collect()
        };
        for (addr, want) in cases {
            let msg = plan
                .osc_in_message(addr, &[Arg::F32(0.25)])
                .unwrap_or_else(|| panic!("{addr}: the boundary dropped the message"));
            route_messages(&mut routes, &mut plan, Inbound::Addressed(&[msg]), None);
            assert_eq!(
                delivered(&routes, &plan),
                [want],
                "{addr}: addressed routing disagrees"
            );

            let (target, arg) = plan.osc_in(addr, &[Arg::F32(0.25)]).expect("resolves");
            let resolved = [Resolved {
                target,
                frame: 0,
                arg,
            }];
            route_messages(&mut routes, &mut plan, Inbound::Resolved(&resolved), None);
            assert_eq!(
                delivered(&routes, &plan),
                [want],
                "{addr}: resolved routing disagrees"
            );
        }
        // An address no node claims resolves nowhere on either path.
        assert!(plan
            .osc_in_message("/nowhere/time", &[Arg::F32(0.25)])
            .is_none());
    }

    /// Test-only Operators for the delivery-order tests below.
    mod probe {
        use crate::descriptor::Descriptor;
        use crate::operator::{Io, Operator};
        use crate::vocab::pitch::{Note, Pitch};
        use alloc::boxed::Box;

        pub mod echo {
            use super::*;
            crate::operator_contract!(EventEcho {
                inputs:  { a: note,
                           b: note,
                           split: f32 { 0.0..=1.0, default 0.0, "", lin } },
                outputs: { out: note },
            });
            /// Re-emits every event it reads, port `a` before port `b`, at its frame, with the
            /// pitch tagged `+100` for port `b`.
            pub struct EventEcho;
            impl Operator for EventEcho {
                fn descriptor() -> Descriptor {
                    Self::contract()
                }
                fn process(&mut self, io: &mut Io) {
                    for (port, tag) in [(IN_A, 0.0), (IN_B, 100.0)] {
                        for ev in io.read(port) {
                            let Pitch::Absolute(p) = ev.payload.pitch else {
                                continue;
                            };
                            io.write(OUT_OUT)
                                .emit(ev.frame, Note::new(Pitch::Absolute(p + tag), 1.0));
                        }
                    }
                }
                fn spawn(&self) -> Box<dyn Operator> {
                    Box::new(EventEcho)
                }
            }
        }
    }

    /// `(frame, pitch)` of each emitted absolute-pitch note.
    fn note_frames<'e>(args: impl Iterator<Item = (usize, &'e Arg)>) -> Vec<(usize, f32)> {
        use crate::vocab::pitch::Pitch;
        args.map(|(frame, arg)| {
            let Arg::Note(n) = arg else {
                panic!("expected a note")
            };
            let Pitch::Absolute(p) = n.pitch else {
                panic!("expected an absolute pitch")
            };
            (frame, p)
        })
        .collect()
    }

    /// Each segment hands a port exactly its own events in the frame window, in delivery order,
    /// with segment-relative frames — interleaved ports, out-of-order frames, same-frame ties and a
    /// held change splitting the block included.
    #[test]
    fn stream_events_reach_their_port_in_delivery_order_per_segment() {
        use crate::op_driver::OpDriver;
        use crate::vocab::pitch::{Note, Pitch};
        use probe::echo::*;

        let note = |p: f32| Note::new(Pitch::Absolute(p), 1.0);
        let mut d = OpDriver::for_type(EventEcho, 48_000.0);
        d.push(IN_A, 10, note(1.0))
            .push(IN_B, 5, note(1.0))
            .push(IN_A, 3, note(2.0))
            .push(IN_B, 40, note(2.0))
            .push(IN_A, 3, note(3.0))
            .push(IN_A, 25, note(4.0))
            .push(IN_SPLIT, 20, 1.0f32)
            .render(64);

        let got = note_frames(d.emits().iter().map(|e| (e.frame, &e.arg)));
        assert_eq!(
            got,
            [
                (10, 1.0),
                (3, 2.0),
                (3, 3.0),
                (5, 101.0),
                (25, 4.0),
                (40, 102.0)
            ]
        );
    }

    /// Hand-built nodes for the prepared-value guards: an Operator emitting one prepared value
    /// on a `str` output (no contract-built Operator can declare one), and one reading a `str`
    /// input. Neither does anything else.
    mod prepared_nodes {
        use alloc::boxed::Box;
        use alloc::sync::Arc;
        use alloc::vec;

        use crate::descriptor::{Descriptor, Port};
        use crate::message::Arg;
        use crate::operator::{Io, MsgWriter, Operator};
        use crate::prepare::{Prepared, Preparer};

        pub(super) fn desc(inputs: vec::Vec<Port>, outputs: vec::Vec<Port>) -> Arc<Descriptor> {
            Arc::new(Descriptor {
                type_name: "hand_built",
                inputs,
                outputs,
                constants: vec![],
                resources: vec![],
            })
        }

        /// Emits `value` once, at frame 0 of its first block.
        pub(super) struct Emitter(pub Option<Prepared>);
        impl Operator for Emitter {
            fn descriptor() -> Descriptor {
                Descriptor {
                    type_name: "hand_built",
                    inputs: vec![],
                    outputs: vec![],
                    constants: vec![],
                    resources: vec![],
                }
            }
            fn process(&mut self, io: &mut Io) {
                if let Some(p) = self.0.take() {
                    MsgWriter::on(io, 0).set(0, Arg::Prepared(p));
                }
            }
            fn spawn(&self) -> Box<dyn Operator> {
                Box::new(Emitter(None))
            }
        }

        pub(super) struct Sink;
        impl Operator for Sink {
            fn descriptor() -> Descriptor {
                Emitter::descriptor()
            }
            fn process(&mut self, _io: &mut Io) {}
            fn spawn(&self) -> Box<dyn Operator> {
                Box::new(Sink)
            }
        }

        pub(super) fn sink(preparer: Preparer) -> Arc<Descriptor> {
            desc(vec![Port::str_prepared("in", preparer)], vec![])
        }
    }

    /// An upstream node's forward of another preparer's value is dropped at the `str` input and
    /// counted, like a send.
    #[test]
    fn a_refused_forward_is_counted() {
        use crate::graph::Graph;
        use crate::prepare::testing::{STEPS, WORDS};
        use prepared_nodes::*;

        let mut g = Graph::new();
        let value = STEPS.prepare(&"1".into()).unwrap();
        let src = g.add_boxed(
            "/src",
            Box::new(Emitter(Some(value))),
            desc(vec![], vec![Port::str_prepared("out", STEPS)]),
        );
        let dst = g.add_boxed("/dst", Box::new(Sink), sink(WORDS));
        g.connect(src, 0usize, dst, 0usize);
        let cfg = AudioConfig::new(48_000.0, 64);
        let mut plan = Plan::instantiate(g, cfg).expect("instantiate");
        let mut r = Renderer::new(&plan);
        let mut master = vec![vec![0.0; cfg.block_size]; plan.config.channels];
        let mut outbound = Vec::new();
        r.render_block_multi(&mut plan, &[], &[], &mut master, &mut outbound);
        assert_eq!(plan.take_dropped_unprepared(), 1);
        assert!(!matches!(plan.nodes[1].latch[0], Arg::Prepared(_)));
    }

    /// A dissolved pipe's two hops each check the kind. No document can give the pipe's port and
    /// its consumer different preparers (Instantiate gives the pipe its consumer's), so the alias
    /// is edited after Instantiate to reach the consumer's own check.
    #[test]
    fn the_consumer_hop_of_a_dissolved_pipe_counts_a_refusal() {
        use crate::descriptor::Descriptor;
        use crate::graph::Graph;
        use crate::operators::pipe::Pipe;
        use crate::prepare::testing::{STEPS, WORDS};
        use alloc::sync::Arc;
        use prepared_nodes::*;

        let mut g = Graph::new();
        let pipe = g.add_boxed(
            "/pat",
            Box::new(Pipe::text()),
            Arc::new(Descriptor {
                type_name: "pipe",
                inputs: vec![Port::str("in")],
                outputs: vec![Port::str("out")],
                constants: vec![],
                resources: vec![],
            }),
        );
        let dst = g.add_boxed("/dst", Box::new(Sink), sink(STEPS));
        g.connect(pipe, 0usize, dst, 0usize);
        let cfg = AudioConfig::new(48_000.0, 64);
        let mut plan = Plan::instantiate(g, cfg).expect("instantiate");
        assert_eq!(plan.input_aliases.len(), 1, "the pipe dissolved");
        plan.input_aliases[0].port = Port::str_prepared("in", WORDS);
        let mut r = Renderer::new(&plan);
        let mut master = vec![vec![0.0; cfg.block_size]; plan.config.channels];
        let mut outbound = Vec::new();
        let words = Arg::Prepared(WORDS.prepare(&"ab".into()).unwrap());
        let messages = [Message::new("/pat/in", words, 0)];
        r.render_block_multi(&mut plan, &messages, &[], &mut master, &mut outbound);
        assert_eq!(plan.take_dropped_unprepared(), 1);
    }

    /// A wired port takes only its upstream node's emissions, in emission order: an external
    /// Message addressed to it is dropped and counted, while one to an unwired port is delivered.
    #[test]
    fn a_wired_port_drops_external_messages_and_counts_them() {
        use crate::graph::Graph;
        use crate::operators::{transpose, OscOut, Transpose};
        use crate::vocab::pitch::{Note, Pitch};
        use probe::echo::*;

        let mut g = Graph::new();
        let src = g.add("/src", Transpose::new());
        let echo = g.add("/echo", EventEcho);
        let sink = g.add("/sink", OscOut::new());
        g.connect(src, transpose::OUT_NOTES, echo, IN_A);
        g.connect(echo, OUT_OUT, sink, 0);
        let cfg = AudioConfig::new(48_000.0, 64);
        let mut plan = Plan::instantiate(g, cfg).expect("instantiate");
        let mut r = Renderer::new(&plan);

        let note = |p: f32| Note::new(Pitch::Absolute(p), 1.0);
        let messages = [
            Message::new("/src/notes", note(10.0), 1),
            Message::new("/echo/a", note(1.0), 5),
            Message::new("/src/notes", note(11.0), 0),
            Message::new("/echo/a", note(2.0), 2),
            Message::new("/src/notes", note(12.0), 5),
        ];
        let mut master = vec![vec![0.0; cfg.block_size]; plan.config.channels];
        let mut outbound = Vec::new();
        r.render_block_multi(&mut plan, &messages, &[], &mut master, &mut outbound);

        let got = note_frames(outbound.iter().map(|m| (m.frame, &m.arg)));
        assert_eq!(got, [(1, 10.0), (0, 11.0), (5, 12.0)]);
        assert_eq!(plan.take_dropped_wired_writes(), 2);
        assert_eq!(
            plan.take_dropped_wired_writes(),
            0,
            "taking the count clears it"
        );
    }

    /// Writes to one Signal input at one frame apply in arrival order, so the last wins. Enough
    /// same-frame writes that the frame sort is not an insertion sort, so dropping the arrival
    /// tie-break would reorder them.
    #[test]
    fn same_frame_materialize_writes_keep_last_write_wins() {
        use crate::graph::Graph;
        use crate::operators::Output;

        let mut g = Graph::new();
        let out = g.add("/out", Output::new());
        g.tap_output(out, 0);
        let cfg = AudioConfig::new(48_000.0, 64);
        let mut plan = Plan::instantiate(g, cfg).expect("instantiate");
        let mut r = Renderer::new(&plan);

        // Writes alternate between frames 10 and 20 with distinct values.
        let messages: Vec<Message> = (0..64)
            .map(|i| Message::float("/out/audio", i as f32, if i % 2 == 0 { 10 } else { 20 }))
            .collect();
        let mut block = vec![0.0; cfg.block_size];
        r.render_block(&mut plan, &messages, &mut block);

        assert!(block[..10].iter().all(|&s| s == 0.0), "{:?}", &block[..10]);
        assert!(
            block[10..20].iter().all(|&s| s == 62.0),
            "frame 10's last write is 62: {:?}",
            &block[10..20]
        );
        assert!(
            block[20..].iter().all(|&s| s == 63.0),
            "frame 20's last write is external 63: {:?}",
            &block[20..]
        );
    }
}
