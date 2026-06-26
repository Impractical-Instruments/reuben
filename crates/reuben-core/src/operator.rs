//! Operator — the authoring contract (ADR-0010, ADR-0030).
//!
//! An Operator is single-Lane: the author writes one mono, single-Voice stream a
//! (sub)block at a time, and the engine fans it out across Lanes with per-Lane state.
//! The process function is allocation-free and sees held values constant for the whole call
//! (the engine block-slices at Message boundaries, ADR-0011), so the author simply reads "my
//! current value".
//!
//! Reads are **two typed verbs** over the one Message model (ADR-0030): [`Io::last`] — the
//! held (zero-order-hold) value on a port — and [`Io::stream`] — the sparse, frame-stamped
//! Messages on a port this (sub)block. Both are generic over the payload type `T` (a
//! [`FromArg`] impl: an OSC primitive, a `&[f32]` buffer, or a *vocab* type). Writes are two
//! verbs: [`Io::emit`] — append one Message to an output port — and [`Io::signal_mut`] — fill
//! this node's own dense output buffer in place.

use std::sync::Arc;

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::message::{Arg, Emit, Event, FromArg};
use crate::resources::{ResolvedRefs, ResourceStore};

/// A typed, frame-stamped payload yielded by [`Io::stream`] — one decoded Message on a port
/// (ADR-0030). `frame` is segment-relative; `payload` is the Message's [`Arg`] decoded to the
/// requested `T` via [`FromArg`].
#[derive(Debug, Clone, Copy)]
pub struct Stamped<T> {
    /// Sample offset within the current (sub)block at which this Message applies.
    pub frame: usize,
    /// The decoded payload.
    pub payload: T,
}

/// The per-call I/O view handed to [`Operator::process`] for one (sub)block of one Lane.
///
/// All dense slices are exactly [`Io::frames`] samples long; held values are constant for the
/// call. The port reference lists are collected into inline [`SmallVec`]s, so building an `Io`
/// allocates nothing for the common low-port-count case. The inline input capacity is sized for
/// the widest operator once its former params became inputs — the sequencer is `clock`, `length`,
/// 16 × `step`, `gate_mode`, `pitch` = 20 — because RT-safety (`rt_safe`) depends on not
/// spilling here on the audio thread.
pub struct Io<'a> {
    sample_rate: f32,
    frames: usize,
    /// Dense per-sample buffer per **input** port (a wired [`Buffer`](Arg::Buffer) source or a
    /// materialized [`F32`](crate::descriptor::PortType::F32) control), or `None` for a port with
    /// no buffer form. Read per-sample via [`Io::signal`].
    inputs: SmallVec<[Option<&'a [f32]>; 20]>,
    outputs: SmallVec<[&'a mut [f32]; 2]>,
    /// The held (ZOH) [`Arg`] per **input** port — the unified per-port latch (ADR-0030),
    /// collapsing the former Harmony / enum / param lanes. In input-port order; `Copy`-normalized
    /// and constant for this (sub)block (the engine block-slices at held-value changes). Read via
    /// [`Io::last`]; empty when unattached, so `last` then reports `None`.
    latched: &'a [Arg],
    /// The sparse [`Event`]s per **input** port this (sub)block, frames segment-relative
    /// (ADR-0030). In input-port order; zero-copy views borrowed from the Render loop. Read via
    /// [`Io::stream`]; empty when unattached.
    streams: &'a [&'a [Event<'a>]],
    lane: usize,
    lanes: usize,
    /// Sink for Messages this call emits (ADR-0014, ADR-0030), or `None` when this Lane does not
    /// collect emissions. Only Lane 0 collects — emission is single-Lane (pre-fan-out). The former
    /// harmony-publish and outbound sinks fold into this: a context/`osc_out` node simply emits to
    /// the right output port, and routing (the wired edge) carries it.
    emit: Option<&'a mut Vec<Emit>>,
    /// Block-absolute frame of this (sub)block's start, added to an emitted frame so the operator
    /// can work in segment-relative time.
    frame_offset: usize,
    /// Per-input `varying` hint (ADR-0030), in input-port order: `false` when a materialized
    /// input held its value unchanged this block, so a const-folding operator may reuse cached
    /// coefficients. Empty when unattached — `varying()` then conservatively reports `true`.
    varying: &'a [bool],
}

impl<'a> Io<'a> {
    /// Internal constructor used by the Render loop. Defaults to a single Lane (lane 0 of
    /// 1); the engine attaches the latch, streams, lane, and emit sink via the builders.
    ///
    /// `inputs`/`outputs` are taken as iterators so the Render loop can wire ports straight
    /// from the arena without an intermediate heap allocation.
    pub(crate) fn new<I, O>(sample_rate: f32, frames: usize, inputs: I, outputs: O) -> Self
    where
        I: IntoIterator<Item = Option<&'a [f32]>>,
        O: IntoIterator<Item = &'a mut [f32]>,
    {
        Self {
            sample_rate,
            frames,
            inputs: inputs.into_iter().collect(),
            outputs: outputs.into_iter().collect(),
            latched: &[],
            streams: &[],
            lane: 0,
            lanes: 1,
            emit: None,
            frame_offset: 0,
            varying: &[],
        }
    }

    /// Attach the per-input held [`Arg`] latch for this segment (ADR-0030). In input-port order;
    /// read by [`Io::last`]. Unattached ⇒ `last()` reports `None`.
    pub(crate) fn with_latched(mut self, latched: &'a [Arg]) -> Self {
        self.latched = latched;
        self
    }

    /// Attach the per-input [`Event`] streams for this (sub)block (ADR-0030). In input-port order;
    /// read by [`Io::stream`]. Unattached ⇒ `stream()` is empty.
    pub(crate) fn with_streams(mut self, streams: &'a [&'a [Event<'a>]]) -> Self {
        self.streams = streams;
        self
    }

    /// Attach the per-input `varying` hints for this segment (ADR-0030). In input-port order;
    /// read by [`Io::varying`]. Unattached ⇒ `varying()` reports `true`.
    pub(crate) fn with_varying(mut self, varying: &'a [bool]) -> Self {
        self.varying = varying;
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

    /// Sample rate in Hz.
    pub fn sample_rate(&self) -> f32 {
        self.sample_rate
    }

    /// Number of samples in this (sub)block.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// **Per-sample read of a buffer input** (ADR-0030): the dense block on `port`. A wired
    /// [`Buffer`](Arg::Buffer) source, or the engine's materialized buffer for an
    /// [`F32`](crate::descriptor::PortType::F32) control filled from its latched value (mid-block
    /// changes written at their frame). Always `frames` long for a migrated port; an empty slice
    /// for a port with neither a wire nor materialization.
    pub fn signal(&self, port: usize) -> &[f32] {
        self.inputs.get(port).copied().flatten().unwrap_or(&[])
    }

    /// **The held (ZOH) value on `port`** (ADR-0030) — the most-recent Message's payload, decoded
    /// to `T`, constant for this (sub)block (the engine block-slices at held-value changes). The
    /// unifying read for scalars, enums, and the Harmony struct: `io.last::<f32>(CUTOFF)`,
    /// `io.last::<SnapDir>(DIR)`, `io.last::<Harmony>(HARMONY)`. `Some(default)` on an input with a
    /// latched default; `None` when nothing is latchable (unwired, no default) or the wire's type
    /// is not a `T`.
    pub fn last<T: FromArg<'a>>(&self, port: usize) -> Option<T> {
        self.latched.get(port).and_then(|arg| T::from_arg(arg))
    }

    /// **The sparse Messages on `port` this (sub)block** (ADR-0030), each decoded to `T` and
    /// frame-stamped (segment-relative). The unifying read for events — a Voicer iterates
    /// `io.stream::<Note>(NOTES)`. Zero-copy; messages whose payload is not a `T` are skipped.
    pub fn stream<T: FromArg<'a>>(&self, port: usize) -> impl Iterator<Item = Stamped<T>> + 'a {
        let events: &'a [Event<'a>] = self.streams.get(port).copied().unwrap_or(&[]);
        events.iter().filter_map(|e| {
            T::from_arg(e.arg).map(|payload| Stamped {
                frame: e.frame,
                payload,
            })
        })
    }

    /// The `varying` hint for an input (ADR-0030): `false` when a materialized input held its
    /// value unchanged this block (so a const-folding op may reuse cached state), `true` when it
    /// is dense or changed this block. Conservatively `true` when unattached.
    pub fn varying(&self, port: usize) -> bool {
        self.varying.get(port).copied().unwrap_or(true)
    }

    /// **Per-sample write view of a buffer output** (ADR-0030): fill this node's own output buffer
    /// on `port` in place. Length == `frames`.
    pub fn signal_mut(&mut self, port: usize) -> &mut [f32] {
        &mut self.outputs[port][..]
    }

    /// **Emit one Message** onto output `port` at segment-relative `frame` (ADR-0014, ADR-0030).
    /// `addr` is the node-local address carried for OSC shape / debug (e.g. `"notes"`); it is
    /// `&'static str` and `payload` is one [`Arg`], so a wired-edge emit allocates nothing. The
    /// engine delivers it as an [`Event`] to nodes downstream of this one in the same block — and,
    /// for an output port wired to the boundary, drains it past the boundary. A no-op on Lanes that
    /// do not collect emissions (every Lane but 0). Replaces the former `publish_harmony` /
    /// `send_outbound`: publishing a Harmony or sending outbound is just an emit to the right port.
    pub fn emit(&mut self, port: usize, addr: &'static str, payload: impl Into<Arg>, frame: usize) {
        let frame = self.frame_offset + frame;
        if let Some(buf) = self.emit.as_mut() {
            buf.push(Emit {
                port,
                address: addr,
                arg: payload.into(),
                frame,
            });
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
