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

/// Owns a Plan + Renderer and serves audio one block at a time into arbitrary buffers.
pub struct Engine {
    plan: Plan,
    renderer: Renderer,
    /// Messages to apply at the start of the next rendered block.
    pending: Vec<Message>,
    /// One block of rendered, not-yet-consumed mono samples.
    scratch: Vec<f32>,
    /// Index of the next unread sample in `scratch`; `>= block_size` means exhausted.
    pos: usize,
}

impl Engine {
    /// Build an engine for `plan` (uses the default serial executor).
    pub fn new(plan: Plan) -> Self {
        let block_size = plan.config.block_size;
        let renderer = Renderer::new(&plan);
        Self {
            plan,
            renderer,
            pending: Vec::new(),
            scratch: vec![0.0; block_size],
            pos: block_size, // exhausted -> first fill renders immediately
        }
    }

    /// The core block size this engine renders in.
    pub fn block_size(&self) -> usize {
        self.plan.config.block_size
    }

    /// Sample rate this engine's Plan was instantiated for.
    pub fn sample_rate(&self) -> f32 {
        self.plan.config.sample_rate
    }

    /// Queue a Message to apply at the start of the next rendered block.
    pub fn queue(&mut self, msg: Message) {
        self.pending.push(msg);
    }

    /// Fill `out` (any length) with mono samples, rendering core blocks as needed.
    pub fn fill(&mut self, out: &mut [f32]) {
        for sample in out.iter_mut() {
            if self.pos >= self.scratch.len() {
                self.render_next();
                self.pos = 0;
            }
            *sample = self.scratch[self.pos];
            self.pos += 1;
        }
    }

    /// Render one block into `scratch`, consuming any queued Messages.
    fn render_next(&mut self) {
        let msgs = std::mem::take(&mut self.pending);
        self.renderer
            .render_block(&mut self.plan, &msgs, &mut self.scratch);
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
        let mut out = vec![0.0f32; 48_000]; // 1 s, not a multiple of block_size
        e.fill(&mut out);
        assert!(peak(&out) > 0.05, "engine produced near-silence");
    }

    #[test]
    fn fill_is_independent_of_chunk_size() {
        // One big fill must equal many ragged fills, sample-for-sample: the engine's
        // block boundary is decoupled from the caller's buffer size.
        let total = 5_000;

        let mut whole = engine_with_note();
        let mut a = vec![0.0f32; total];
        whole.fill(&mut a);

        let mut chunked = engine_with_note();
        let mut b = vec![0.0f32; total];
        let mut i = 0;
        for step in [37usize, 256, 1, 500, 129].iter().cycle() {
            if i >= total {
                break;
            }
            let end = (i + step).min(total);
            chunked.fill(&mut b[i..end]);
            i = end;
        }

        for (k, (x, y)) in a.iter().zip(&b).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "mismatch at sample {k}");
        }
    }

    #[test]
    fn queued_messages_are_consumed_once() {
        // After a block renders, the queue is empty (the note isn't re-sent every block).
        let mut e = engine_with_note();
        let mut out = vec![0.0f32; e.block_size()];
        e.fill(&mut out);
        assert!(e.pending.is_empty(), "pending messages not drained");
    }
}
