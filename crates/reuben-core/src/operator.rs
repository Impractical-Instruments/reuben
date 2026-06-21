//! Operator — the authoring contract (ADR-0010).
//!
//! An Operator is single-Lane: the author writes one mono, single-Voice stream a
//! (sub)block at a time, and the engine fans it out across Lanes with per-Lane state.
//! The process function is allocation-free and sees params held constant for the whole
//! call (the engine block-slices at Message boundaries, ADR-0011), so the author simply
//! reads "my current value". Event-oriented operators (the Voicer) instead read the raw
//! Message list via [`Io::messages`].

use crate::descriptor::Descriptor;
use crate::message::Message;

/// The per-call I/O view handed to [`Operator::process`] for one (sub)block of one Lane.
///
/// All slices are exactly [`Io::frames`] samples long. Params are constant for the call.
pub struct Io<'a> {
    sample_rate: f32,
    frames: usize,
    inputs: &'a [Option<&'a [f32]>],
    outputs: &'a mut [&'a mut [f32]],
    params: &'a [f32],
    messages: &'a [Message],
    lane: usize,
    lanes: usize,
}

impl<'a> Io<'a> {
    /// Internal constructor used by the Render loop. Defaults to a single Lane (lane 0 of
    /// 1); the engine sets the real Lane via [`Io::with_lane`] when replicating.
    pub(crate) fn new(
        sample_rate: f32,
        frames: usize,
        inputs: &'a [Option<&'a [f32]>],
        outputs: &'a mut [&'a mut [f32]],
        params: &'a [f32],
        messages: &'a [Message],
    ) -> Self {
        Self {
            sample_rate,
            frames,
            inputs,
            outputs,
            params,
            messages,
            lane: 0,
            lanes: 1,
        }
    }

    /// Set which Lane (Voice) of how many this call is, for replicated operators.
    pub(crate) fn with_lane(mut self, lane: usize, lanes: usize) -> Self {
        self.lane = lane;
        self.lanes = lanes;
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

    /// Raw Messages for this (sub)block, frames relative to the segment start.
    /// Used by event operators such as the Voicer.
    pub fn messages(&self) -> &[Message] {
        self.messages
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
