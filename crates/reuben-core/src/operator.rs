//! Operator — the authoring contract (ADR-0010).
//!
//! An Operator is single-Lane: the author writes one mono, single-Voice stream a
//! (sub)block at a time, and the engine fans it out across Lanes with per-Lane state.
//! The process function is allocation-free and sees params held constant for the whole
//! call (the engine block-slices at Message boundaries, ADR-0011), so the author simply
//! reads "my current value". Event-oriented operators (the Voicer) instead read the
//! routed [`Event`] list via [`Io::events`].

use std::sync::Arc;

use smallvec::SmallVec;

use crate::context::Context;
use crate::descriptor::Descriptor;
use crate::message::{Arg, Emit, Event, Outbound};
use crate::resources::{ResolvedRefs, ResourceStore};

/// A tonal-[`Context`] snapshot an operator publishes during `process` onto a Context output
/// port (ADR-0015), before the engine routes it to downstream readers' context slices.
/// Sibling of [`Emit`]; `ctx` is `Copy`, so the engine snapshots it allocation-free.
#[derive(Debug, Clone, Copy)]
pub struct CtxPublish {
    /// Context-output ordinal (separate index space from Signal/Message outputs).
    pub port: usize,
    /// Sample offset within the block. Segment-relative when the operator calls
    /// `publish_context`; the engine stamps it block-absolute.
    pub frame: usize,
    pub ctx: Context,
}

/// The per-call I/O view handed to [`Operator::process`] for one (sub)block of one Lane.
///
/// All slices are exactly [`Io::frames`] samples long. Params are constant for the call.
/// The port reference lists are collected into inline [`SmallVec`]s, so building an `Io`
/// allocates nothing for the common low-port-count case (≤4 inputs, ≤2 outputs).
pub struct Io<'a> {
    sample_rate: f32,
    frames: usize,
    inputs: SmallVec<[Option<&'a [f32]>; 4]>,
    outputs: SmallVec<[&'a mut [f32]; 2]>,
    params: &'a [f32],
    events: &'a [Event<'a>],
    lane: usize,
    lanes: usize,
    /// Sink for Messages this call emits (ADR-0014), or `None` when this Lane does not
    /// collect emissions. Only Lane 0 collects — emission is single-Lane (pre-fan-out).
    emit: Option<&'a mut Vec<Emit>>,
    /// Resolved tonal [`Context`] for each Context **input** port this segment (ADR-0015),
    /// in context-input ordinal order. Constant for the call (the engine slices at context
    /// changes). Empty for operators with no Context inputs; borrowed from the Render loop.
    contexts: &'a [Context],
    /// Sink for Context snapshots this call publishes (ADR-0015), or `None` when this Lane
    /// does not publish. Like `emit`, single-Lane (the context node is pre-fan-out).
    ctx_publish: Option<&'a mut Vec<CtxPublish>>,
    /// Sink for boundary-bound Messages this call sends out (ADR-0026) — the outbound route, or
    /// `None` when this Lane does not collect. Like `emit`, single-Lane (the sink is pre-fan-out);
    /// an `osc_out` op forwards its input events here and the engine drains them past the boundary.
    outbound: Option<&'a mut Vec<Outbound>>,
    /// Block-absolute frame of this (sub)block's start, added to an emitted/published frame
    /// so the operator can work in segment-relative time.
    frame_offset: usize,
    /// Per-input `varying` hint (ADR-0028), in input-port order: `false` when a materialized
    /// [`Shape::Float`](crate::descriptor::Shape) input held its value unchanged this block, so a
    /// const-folding operator may reuse cached coefficients. Empty when unattached — `varying()`
    /// then conservatively reports `true` (always recompute), which a naive operator ignores.
    varying: &'a [bool],
    /// Held [`Shape::Enum`](crate::descriptor::Shape) value per input port (ADR-0028), as the
    /// variant **index**, constant for this (sub)block (the engine block-slices at enum changes).
    /// In input-port order; `0` for non-enum / unconnected ports. Read via [`Io::enum_index`].
    enums: &'a [usize],
}

impl<'a> Io<'a> {
    /// Internal constructor used by the Render loop. Defaults to a single Lane (lane 0 of
    /// 1); the engine sets the real Lane via [`Io::with_lane`] when replicating.
    ///
    /// `inputs`/`outputs` are taken as iterators so the Render loop can wire ports straight
    /// from the arena without an intermediate heap allocation.
    pub(crate) fn new<I, O>(
        sample_rate: f32,
        frames: usize,
        inputs: I,
        outputs: O,
        params: &'a [f32],
        events: &'a [Event<'a>],
    ) -> Self
    where
        I: IntoIterator<Item = Option<&'a [f32]>>,
        O: IntoIterator<Item = &'a mut [f32]>,
    {
        Self {
            sample_rate,
            frames,
            inputs: inputs.into_iter().collect(),
            outputs: outputs.into_iter().collect(),
            params,
            events,
            lane: 0,
            lanes: 1,
            emit: None,
            contexts: &[],
            ctx_publish: None,
            outbound: None,
            frame_offset: 0,
            varying: &[],
            enums: &[],
        }
    }

    /// Attach the per-input `varying` hints for this segment (ADR-0028). In input-port order;
    /// read by [`Io::varying`]. Unattached ⇒ `varying()` reports `true`.
    pub(crate) fn with_varying(mut self, varying: &'a [bool]) -> Self {
        self.varying = varying;
        self
    }

    /// Attach the held [`Shape::Enum`](crate::descriptor::Shape) values for this segment (ADR-0028),
    /// in input-port order. Read by [`Io::enum_index`]. Unattached ⇒ `enum_index()` reports `0`.
    pub(crate) fn with_enums(mut self, enums: &'a [usize]) -> Self {
        self.enums = enums;
        self
    }

    /// Set which Lane (Voice) of how many this call is, for replicated operators.
    pub(crate) fn with_lane(mut self, lane: usize, lanes: usize) -> Self {
        self.lane = lane;
        self.lanes = lanes;
        self
    }

    /// Attach the emit sink and segment frame offset (Lane 0 only). Messages passed to
    /// [`Io::emit`] are collected into `buf` with `frame_offset` added.
    pub(crate) fn with_emit(mut self, buf: &'a mut Vec<Emit>, frame_offset: usize) -> Self {
        self.emit = Some(buf);
        self.frame_offset = frame_offset;
        self
    }

    /// Set the resolved Context for each Context input port this segment (ADR-0015).
    pub(crate) fn with_contexts(mut self, contexts: &'a [Context]) -> Self {
        self.contexts = contexts;
        self
    }

    /// Attach the context-publish sink and segment frame offset (Lane 0 only). Snapshots
    /// passed to [`Io::publish_context`] are collected into `buf` with `frame_offset` added.
    pub(crate) fn with_context_publish(
        mut self,
        buf: &'a mut Vec<CtxPublish>,
        frame_offset: usize,
    ) -> Self {
        self.ctx_publish = Some(buf);
        self.frame_offset = frame_offset;
        self
    }

    /// Attach the outbound-route sink and segment frame offset (Lane 0 only). Messages passed to
    /// [`Io::send_outbound`] are collected into `buf` with `frame_offset` added (ADR-0026).
    pub(crate) fn with_outbound(mut self, buf: &'a mut Vec<Outbound>, frame_offset: usize) -> Self {
        self.outbound = Some(buf);
        self.frame_offset = frame_offset;
        self
    }

    /// Sample rate in Hz.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Number of samples in this (sub)block.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Borrow an input Signal port, or `None` if unconnected.
    pub fn input(&self, port: usize) -> Option<&[f32]> {
        self.inputs.get(port).copied().flatten()
    }

    /// **Per-sample read view of a [`Shape::Float`](crate::descriptor::Shape) input** (ADR-0028).
    /// Always a buffer `frames` long: the wired source when connected, else the engine's
    /// materialized buffer filled from the input's latched default (with mid-block changes written
    /// at their frame). The single read path that replaces the old
    /// `io.input(..).map_or(io.param(..), ..)` two-step. Returns an empty slice only for a port
    /// that has neither a wire nor materialization (a not-yet-migrated input); migrated operators
    /// always get `frames` samples.
    pub fn signal(&self, port: usize) -> &[f32] {
        self.inputs.get(port).copied().flatten().unwrap_or(&[])
    }

    /// **Block-rate / scalar read view of a [`Shape::Float`](crate::descriptor::Shape) input**
    /// (ADR-0028) — the latched current value at this segment's start, for operators that do not
    /// process per-sample (a clock reading tempo, a sample-and-hold). Reads the head of the
    /// materialized buffer without looping it.
    pub fn value(&self, port: usize) -> f32 {
        self.signal(port).first().copied().unwrap_or(0.0)
    }

    /// The `varying` hint for a [`Shape::Float`](crate::descriptor::Shape) input (ADR-0028):
    /// `false` when a materialized input held its value unchanged this block (so a const-folding
    /// op may reuse cached state), `true` when it is dense or changed this block. Conservatively
    /// `true` when unattached — a naive operator ignores it and reads `signal()[i]`.
    pub fn varying(&self, port: usize) -> bool {
        self.varying.get(port).copied().unwrap_or(true)
    }

    /// Borrow an output Signal port for writing (length == `frames`).
    pub fn output(&mut self, port: usize) -> &mut [f32] {
        &mut self.outputs[port][..]
    }

    /// **Per-sample write view of a [`Shape::Float`](crate::descriptor::Shape) output** (ADR-0028)
    /// — the forward-looking name for [`Io::output`]. Length == `frames`.
    pub fn signal_mut(&mut self, port: usize) -> &mut [f32] {
        self.output(port)
    }

    /// Current value of a param slot (constant for this call).
    pub fn param(&self, slot: usize) -> f32 {
        self.params[slot]
    }

    /// **Held read view of an [`Shape::Enum`](crate::descriptor::Shape) input** (ADR-0028) — the
    /// current variant **index**, constant for this (sub)block (the engine block-slices at enum
    /// changes, so an operator sees one choice per call). The operator maps it to its generated
    /// enum type, e.g. `Waveform::from_index(io.enum_index(IN_WAVEFORM)).unwrap_or_default()`.
    /// Returns the input's latched default (or `0` when unattached / non-enum).
    pub fn enum_index(&self, port: usize) -> usize {
        self.enums.get(port).copied().unwrap_or(0)
    }

    /// Routed [`Event`]s for this (sub)block, frames relative to the segment start.
    /// Used by event operators such as the Voicer. Zero-copy views (no allocation).
    pub fn events(&self) -> &[Event<'_>] {
        self.events
    }

    /// Emit a Message onto Message output `port` at segment-relative `frame` (ADR-0014).
    /// `addr` is the node-local address the destination matches (e.g. `"note"`); it is
    /// `&'static str`, so a wired-edge emit allocates nothing. The engine delivers it as an
    /// [`Event`] to nodes downstream of this one in the same block. A no-op on Lanes that
    /// do not collect emissions (every Lane but 0).
    pub fn emit(
        &mut self,
        port: usize,
        addr: &'static str,
        args: impl IntoIterator<Item = Arg>,
        frame: usize,
    ) {
        let frame = self.frame_offset + frame;
        if let Some(buf) = self.emit.as_mut() {
            buf.push(Emit {
                port,
                addr,
                args: args.into_iter().collect(),
                frame,
            });
        }
    }

    /// Send a Message past the boundary on the outbound route (ADR-0026). The engine stamps it
    /// block-absolute and with this node's address (the outbound OSC address), then drains it to
    /// native's UDP sender. **Message-domain only**; carries no address (the sink is address-fixed,
    /// so the wiring is the node). A no-op on Lanes that do not collect (every Lane but 0).
    pub fn send_outbound(&mut self, args: impl IntoIterator<Item = Arg>, frame: usize) {
        let frame = self.frame_offset + frame;
        if let Some(buf) = self.outbound.as_mut() {
            buf.push(Outbound {
                args: args.into_iter().collect(),
                frame,
            });
        }
    }

    /// The current tonal [`Context`] on Context input `port` (ADR-0015) — the latched
    /// "what's the key/chord right now", constant for this (sub)block. Returns the default
    /// (C major, 12-TET) when `port` is unconnected, so a degree resolves identically to the
    /// prior 12-TET behavior in a rig with no context node.
    pub fn context(&self, port: usize) -> Context {
        self.contexts.get(port).copied().unwrap_or_default()
    }

    /// The current [`Harmony`](crate::descriptor::Shape::Harmony) on a held-struct input
    /// (ADR-0028) — the forward-looking name for [`Io::context`]. Same latched read service; the
    /// [`Context`] struct is renamed `Harmony` once the carrier vocabulary is retired.
    pub fn harmony(&self, port: usize) -> Context {
        self.context(port)
    }

    /// Publish a tonal [`Context`] snapshot onto Context output `port` at segment-relative
    /// `frame` (ADR-0015). The engine latches it (shared, persistent across blocks) and
    /// re-slices downstream readers at `frame`, so a chord/key change is sample-accurate on
    /// the same timeline as notes. A no-op on Lanes that do not publish (every Lane but 0).
    pub fn publish_context(&mut self, port: usize, frame: usize, ctx: Context) {
        let frame = self.frame_offset + frame;
        if let Some(buf) = self.ctx_publish.as_mut() {
            buf.push(CtxPublish { port, frame, ctx });
        }
    }

    /// Which Lane (Voice) this call represents, in `0..lanes()`. Single-Lane operators can
    /// ignore it; an expander like the Voicer uses it to emit just this Voice's output.
    pub fn lane(&self) -> usize {
        self.lane
    }

    /// Total Lane (Voice) count at this point in the graph.
    pub fn lanes(&self) -> usize {
        self.lanes
    }
}

/// A unit of behavior. Authored single-Lane; replicated across Lanes by the engine.
pub trait Operator: Send {
    /// Static self-description (ports + param metadata). Drives serialization,
    /// connection checking, good-button controls, and AI grounding.
    fn descriptor() -> Descriptor
    where
        Self: Sized;

    /// Process exactly one (sub)block for one Lane. Must not allocate.
    fn process(&mut self, io: &mut Io);

    /// Make a fresh-state instance of the same operator type, for the engine to use as
    /// another Voice's Lane. Params are applied by the engine separately, so this only
    /// needs to reset per-Lane state (typically `Box::new(Self::new())`). An operator that
    /// holds a resource binding (see [`Operator::bind_resources`]) must carry it forward
    /// here while resetting playback state, so every Voice shares the decoded data.
    fn spawn(&self) -> Box<dyn Operator>;

    /// Receive decoded resources after construction, before Plan fan-out (ADR-0016). The
    /// loader calls this on every node that declares a resource slot in its descriptor,
    /// handing the shared [`ResourceStore`] (clone the `Arc` to hold it) and the node's
    /// [`ResolvedRefs`] (resolved handles by slot name). Default no-op — the two-phase
    /// init pattern for a type-erased registry, so operators with no resources ignore it.
    fn bind_resources(&mut self, _store: &Arc<ResourceStore>, _refs: &ResolvedRefs) {}
}
