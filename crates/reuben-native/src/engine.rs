//! Engine — bridges the fixed block-size core to a real-time, arbitrary-length pull.
//!
//! A cpal callback asks for an arbitrary number of frames at unpredictable times; the
//! core [`Renderer`] produces exactly `block_size` samples per call. [`Engine`] owns the
//! Plan + Renderer and a small scratch block, rendering a fresh block whenever the
//! scratch is drained. Incoming external Messages are queued and applied at the start of
//! the next rendered block — **block-quantized by design**: their UDP arrival jitter
//! dwarfs sample resolution, so a finer frame would be fake precision (see [`crate::osc`]).
//! Sample-accurate timing comes from inside the graph (the Clock), not from this queue.
//!
//! NOTE (RT-debt): [`Renderer::render_block`] is allocation-free, but [`Engine::fill`]'s
//! message handoff (the `pending` Vec) still churns the heap when messages flow, so the
//! audio callback isn't fully allocation-free yet. A lock-free, preallocated handoff is
//! tracked for later.

use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;

/// Owns a Plan + Renderer and serves **interleaved logical** audio one block at a time into
/// arbitrary buffers. "Logical" = the instrument's master channels (ADR-0026); mapping those
/// onto the real device's channel count is `audio.rs`'s job, not the engine's.
pub struct Engine {
    plan: Plan,
    renderer: Renderer,
    /// Messages to apply at the start of the next rendered block.
    pending: Vec<Message>,
    /// Outbound Messages an `osc_out` sink sent (ADR-0026), accumulated across the block(s) one
    /// [`Engine::fill`] renders and drained by the caller after fill (native encodes + UDP-sends
    /// them). Cleared at the top of each `fill`.
    outbound: Vec<Message>,
    /// Logical master channel count, fixed for this Plan.
    channels: usize,
    /// One block of rendered, not-yet-consumed samples, planar: `scratch[channel][frame]`.
    scratch: Vec<Vec<f32>>,
    /// Index of the next unread frame in `scratch`; `>= block_size` means exhausted.
    pos: usize,
}

impl Engine {
    /// Build an engine for `plan` (uses the default serial executor).
    pub fn new(plan: Plan) -> Self {
        let block_size = plan.config.block_size;
        let channels = plan.config.channels;
        let renderer = Renderer::new(&plan);
        Self {
            plan,
            renderer,
            pending: Vec::new(),
            outbound: Vec::new(),
            channels,
            scratch: vec![vec![0.0; block_size]; channels],
            pos: block_size, // exhausted -> first fill renders immediately
        }
    }

    /// The core block size this engine renders in.
    pub fn block_size(&self) -> usize {
        self.plan.config.block_size
    }

    /// Logical master channel count (ADR-0026). `fill` interleaves this many channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Sample rate this engine's Plan was instantiated for.
    pub fn sample_rate(&self) -> f32 {
        self.plan.config.sample_rate
    }

    /// Queue a Message to apply at the start of the next rendered block.
    pub fn queue(&mut self, msg: Message) {
        self.pending.push(msg);
    }

    /// Drain the outbound Messages produced by the most recent [`Engine::fill`] (ADR-0026), in
    /// emission order. The caller (native's OSC-out path) encodes and UDP-sends them. Empty unless
    /// the instrument has an `osc_out` sink that fired; call right after `fill`, before the next.
    pub fn drain_outbound(&mut self) -> std::vec::Drain<'_, Message> {
        self.outbound.drain(..)
    }

    /// Fill `out` with **interleaved logical** samples, rendering core blocks as needed.
    /// `out.len()` must be a multiple of [`Engine::channels`]; frame `f`, channel `c` lands at
    /// `out[f * channels + c]`.
    pub fn fill(&mut self, out: &mut [f32]) {
        let ch = self.channels;
        debug_assert_eq!(
            out.len() % ch,
            0,
            "fill buffer must be a multiple of channels"
        );
        let frames = out.len() / ch;
        // Fresh outbound collection for this fill; the render path appends, the caller drains.
        self.outbound.clear();
        for f in 0..frames {
            if self.pos >= self.block_size() {
                self.render_next();
                self.pos = 0;
            }
            for c in 0..ch {
                out[f * ch + c] = self.scratch[c][self.pos];
            }
            self.pos += 1;
        }
    }

    /// Render one block into `scratch`, consuming any queued Messages.
    fn render_next(&mut self) {
        let msgs = std::mem::take(&mut self.pending);
        self.renderer.render_block_multi(
            &mut self.plan,
            &msgs,
            &mut self.scratch,
            &mut self.outbound,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rigs::default_rig;
    use reuben_core::message::{Arg, Message};
    use reuben_core::AudioConfig;

    fn engine_with_note() -> Engine {
        let cfg = AudioConfig::new(48_000.0, 256);
        let plan = Plan::instantiate(default_rig(), cfg).expect("instantiate");
        let mut e = Engine::new(plan);
        e.queue(Message::new(
            "/voicer/note",
            [Arg::Float(69.0), Arg::Float(1.0)],
            0,
        ));
        e
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
    }

    #[test]
    fn default_rig_makes_a_tone() {
        let mut e = engine_with_note();
        // The default rig is mono (broadcast) -> floors to 2 logical channels.
        assert_eq!(e.channels(), 2);
        let mut out = vec![0.0f32; 48_000]; // ~0.5 s interleaved stereo, not a block multiple
        e.fill(&mut out);
        assert!(peak(&out) > 0.05, "engine produced near-silence");
    }

    #[test]
    fn fill_is_independent_of_chunk_size() {
        // One big fill must equal many ragged fills, sample-for-sample: the engine's block
        // boundary is decoupled from the caller's buffer size. Chunk sizes are in *frames*,
        // scaled to interleaved samples so each fill lands on a frame boundary.
        let ch = engine_with_note().channels();
        let total_frames = 5_000;
        let total = total_frames * ch;

        let mut whole = engine_with_note();
        let mut a = vec![0.0f32; total];
        whole.fill(&mut a);

        let mut chunked = engine_with_note();
        let mut b = vec![0.0f32; total];
        let mut i = 0;
        for step_frames in [37usize, 256, 1, 500, 129].iter().cycle() {
            if i >= total {
                break;
            }
            let end = (i + step_frames * ch).min(total);
            chunked.fill(&mut b[i..end]);
            i = end;
        }

        for (k, (x, y)) in a.iter().zip(&b).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "mismatch at sample {k}");
        }
    }

    /// A rig with a normal audio path (for a master tap) plus an `osc_out` sink at `/fb`.
    fn osc_out_plan() -> Plan {
        use reuben_core::graph::Graph;
        use reuben_core::operators::{OscOut, Oscillator, Output};
        let mut g = Graph::new();
        let osc = g.add("/osc", Oscillator::new());
        let out = g.add("/out", Output::new());
        g.connect(osc, 0, out, 0);
        g.tap_output(out, 0);
        g.add("/fb", OscOut::new());
        Plan::instantiate(g, AudioConfig::new(48_000.0, 256)).expect("instantiate")
    }

    #[test]
    fn outbound_messages_drain_after_fill() {
        // A value addressed to the sink's node routes in and comes back out on the outbound
        // route, stamped with the node address (ADR-0026).
        let mut e = Engine::new(osc_out_plan());
        e.queue(Message::new("/fb", [Arg::Float(0.7)], 0));
        let mut out = vec![0.0f32; e.block_size() * e.channels()];
        e.fill(&mut out);

        let drained: Vec<_> = e.drain_outbound().collect();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].addr, "/fb");
        assert_eq!(drained[0].args.as_slice(), &[Arg::Float(0.7)]);
        // Drained once: the next fill (no input) yields nothing.
        e.fill(&mut out);
        assert_eq!(e.drain_outbound().count(), 0);
    }

    #[test]
    fn queued_messages_are_consumed_once() {
        // After a block renders, the queue is empty (the note isn't re-sent every block).
        let mut e = engine_with_note();
        let mut out = vec![0.0f32; e.block_size() * e.channels()];
        e.fill(&mut out);
        assert!(e.pending.is_empty(), "pending messages not drained");
    }
}
