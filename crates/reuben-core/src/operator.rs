//! Operator — the authoring contract (ADR-0010).
//!
//! An Operator is single-Lane: the author writes one mono, single-Voice stream a
//! (sub)block at a time, and the engine fans it out across Lanes with per-Lane state.
//! The process function is allocation-free and sees params held constant for the whole
//! call (the engine block-slices at Message boundaries, ADR-0011), so the author simply
//! reads "my current value". Event-oriented operators (the Voicer) instead read the
//! routed [`Event`] list via [`Io::events`].

use smallvec::SmallVec;

use crate::descriptor::Descriptor;
use crate::message::{Arg, Emit, Event};

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
    /// Block-absolute frame of this (sub)block's start, added to an emitted frame so the
    /// operator can work in segment-relative time.
    frame_offset: usize,
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
            frame_offset: 0,
        }
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

    /// Borrow an input Signal port, or `None` if unconnected.
    pub fn input(&self, port: usize) -> Option<&[f32]> {
        self.inputs.get(port).copied().flatten()
    }

    /// Borrow an output Signal port for writing (length == `frames`).
    pub fn output(&mut self, port: usize) -> &mut [f32] {
        &mut self.outputs[port][..]
    }

    /// Current value of a param slot (constant for this call).
    pub fn param(&self, slot: usize) -> f32 {
        self.params[slot]
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
    /// needs to reset per-Lane state (typically `Box::new(Self::new())`).
    fn spawn(&self) -> Box<dyn Operator>;
}
