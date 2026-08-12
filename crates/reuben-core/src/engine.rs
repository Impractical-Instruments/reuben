//! Engine — bridges the fixed block-size core to a real-time, arbitrary-length pull.
//!
//! A host audio callback (cpal, a WebAudio worklet quantum, a game engine's mix step) asks for
//! an arbitrary number of frames at unpredictable times; the core [`Renderer`] produces exactly
//! `block_size` samples per call. [`Engine`] owns the Plan + Renderer and a small scratch block,
//! rendering a fresh block whenever the scratch is drained. Incoming external Messages are
//! queued and applied at the start of the next rendered block — **block-quantized by design**:
//! their arrival jitter (UDP, `postMessage`, …) dwarfs sample resolution, so a finer frame would
//! be fake precision. Sample-accurate timing comes from inside the graph (the Clock), not from
//! this queue.
//!
//! This module is the shared **embed surface**: every shell (native, web, game) constructs an
//! Engine — usually via `reuben-document`'s `from_document` — then drives `queue_osc` → `fill` /
//! `fill_duplex` → `drain_outbound`. Protocol decode (UDP/OSC datagrams, worklet message
//! buffers) stays in the shells; the Engine takes the already-flat primitive args.
//!
//! NOTE (RT-debt): [`Renderer::render_block`] is allocation-free, but [`Engine::fill`]'s
//! message handoff (the `pending` Vec) still churns the heap when messages flow, so the
//! audio callback isn't fully allocation-free yet. A lock-free, preallocated handoff is
//! tracked for later.
//!
//! see rules: execution-runtime

use crate::message::{Arg, Message};
use crate::plan::Plan;
use crate::render::Renderer;

/// Owns a Plan + Renderer and serves **interleaved logical** audio one block at a time into
/// arbitrary buffers. "Logical" = the instrument's master channels; mapping those
/// onto the real device's channel count is the shell's job (native's `audio.rs`), not the
/// engine's.
pub struct Engine {
    plan: Plan,
    renderer: Renderer,
    /// Messages to apply at the start of the next rendered block.
    pending: Vec<Message>,
    /// Outbound Messages an `osc_out` sink sent, accumulated across the block(s) one
    /// [`Engine::fill`] renders and drained by the caller after fill (native encodes + UDP-sends
    /// them). Cleared at the top of each `fill`.
    outbound: Vec<Message>,
    /// Logical master channel count, fixed for this Plan.
    channels: usize,
    /// Logical **input** channel count, fixed for this Plan; `0` for a patch
    /// that binds no input channels — the common case, which pays nothing below.
    in_channels: usize,
    /// One block of rendered, not-yet-consumed samples, planar: `scratch[channel][frame]`.
    scratch: Vec<Vec<f32>>,
    /// One block of staged logical input, planar: `in_scratch[channel][frame]` — the dual of
    /// `scratch`, filled by [`Engine::fill_duplex`] and consumed by the next rendered block.
    /// Empty when `in_channels == 0`. Preallocated here; never grown on the audio thread.
    in_scratch: Vec<Vec<f32>>,
    /// `true` once a [`Engine::fill_duplex`] call staged real (non-empty) input, i.e.
    /// `in_scratch` may hold nonzero samples. While `false` — the entire pre-input life of a
    /// patch driven through [`Engine::fill`] — the no-input path skips staging altogether, so
    /// an input-bound patch pays nothing per frame in the device callback.
    in_dirty: bool,
    /// Index of the next unread frame in `scratch`; `>= block_size` means exhausted.
    pos: usize,
}

impl Engine {
    /// Build an engine for `plan` (uses the default serial executor).
    pub fn new(plan: Plan) -> Self {
        let block_size = plan.config.block_size;
        let channels = plan.config.channels;
        let in_channels = plan.config.input_channels;
        let renderer = Renderer::new(&plan);
        Self {
            plan,
            renderer,
            pending: Vec::new(),
            outbound: Vec::new(),
            channels,
            in_channels,
            scratch: vec![vec![0.0; block_size]; channels],
            in_scratch: vec![vec![0.0; block_size]; in_channels],
            in_dirty: false,
            pos: block_size, // exhausted -> first fill renders immediately
        }
    }

    /// How many queued inbound messages are still waiting for the next block. A host draining
    /// control into the engine can see whether its queue has been consumed; `fill` empties it, so
    /// this is zero at every block boundary in steady state.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// The core block size this engine renders in.
    pub fn block_size(&self) -> usize {
        self.plan.config.block_size
    }

    /// Logical master channel count. `fill` interleaves this many channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Logical **input** channel count. [`Engine::fill_duplex`] de-interleaves
    /// this many channels from its `input`; `0` (a patch with no bound input pipes) means
    /// input is ignored entirely.
    pub fn input_channels(&self) -> usize {
        self.in_channels
    }

    /// Sample rate this engine's Plan was instantiated for.
    pub fn sample_rate(&self) -> f32 {
        self.plan.config.sample_rate
    }

    /// Queue a Message to apply at the start of the next rendered block.
    pub fn queue(&mut self, msg: Message) {
        self.pending.push(msg);
    }

    /// Queue an inbound external message in **flat primitive form**: an address plus
    /// the flat `F32`/`I32`/`Str` args, however the shell decoded them (a UDP/OSC datagram, a
    /// worklet control buffer). Converts them into the single typed [`Message`] the destination
    /// port carries (driven by the port's Arg type), then queues it. Dropped silently if the
    /// address routes to no node/port or the args don't fit — an authoring error the boundary
    /// already tolerates. The conversion needs the Plan, which the engine owns, so it lives here
    /// rather than in the address-blind decode layers.
    pub fn queue_osc(&mut self, address: &str, args: &[Arg]) {
        if let Some(msg) = self.plan.osc_in_message(address, args) {
            self.pending.push(msg);
        }
    }

    /// Drain the outbound Messages produced by the most recent [`Engine::fill`], in
    /// emission order. The caller (native's OSC-out path) encodes and UDP-sends them. Empty unless
    /// the instrument has an `osc_out` sink that fired; call right after `fill`, before the next.
    pub fn drain_outbound(&mut self) -> std::vec::Drain<'_, Message> {
        self.outbound.drain(..)
    }

    /// Transplant survivor operator boxes from a `retiring` Engine into this (freshly built) one,
    /// per the Coordinator's precomputed survivor pairs — the `(old, new)` slice a
    /// [`MigrationTable`](crate::coordinator::MigrationTable) yields via `survivors()`.
    /// Engine owns two Plans here (fresh + retiring) but no longer indexes their nodes: it forwards
    /// to the pointer-swap primitive that lives on [`Plan`] (which owns its nodes). See
    /// [`Plan::transplant_survivors`] for the box-move semantics, the pairing invariant, and the
    /// RT-safety guarantee (a bounded `mem::swap` loop — no alloc/drop/lock). Runs at the
    /// render-side install slot's callback top.
    pub fn transplant_survivors(&mut self, retiring: &mut Engine, pairs: &[(usize, usize)]) {
        self.plan.transplant_survivors(&mut retiring.plan, pairs);
    }

    /// Fill `out` with **interleaved logical** samples, rendering core blocks as needed.
    /// `out.len()` must be a multiple of [`Engine::channels`]; frame `f`, channel `c` lands at
    /// `out[f * channels + c]`. The no-input convenience: an instrument's bound input pipes
    /// fall back to their declared defaults (a bare pipe reads silence) — use
    /// [`Engine::fill_duplex`] to supply the logical input.
    pub fn fill(&mut self, out: &mut [f32]) {
        self.fill_duplex(&[], out);
    }

    /// [`Engine::fill`] with the **logical input master** supplied: `input` is
    /// interleaved at [`Engine::input_channels`] channels and carries **one input frame per
    /// output frame** (`input.len() / input_channels == out.len() / channels`; same clock —
    /// the device layer resamples before this seam). A short `input` stages
    /// zeros for the missing samples — dark-degrade. A caller with no input stream can
    /// always pass `&[]`: before any input has ever been staged that is the true no-input
    /// path (bound pipes fall back to their declared defaults); after, it stages honest
    /// device silence.
    ///
    /// **Alignment:** input is staged one core block ahead — the block rendered at global
    /// frame `k·B` consumes input frames `[(k-1)·B, k·B)` (the first block reads silence).
    /// One block of input latency is what makes the pull **causal** (a block renders only
    /// after all of its input frames have arrived, whatever the caller's chunk size) and
    /// keeps output a pure function of the global frame index — chunk-size independent, like
    /// the output side.
    pub fn fill_duplex(&mut self, input: &[f32], out: &mut [f32]) {
        let ch = self.channels;
        let in_ch = self.in_channels;
        debug_assert_eq!(
            out.len() % ch,
            0,
            "fill buffer must be a multiple of channels"
        );
        // The input stride pin, mirroring the output-side assert: `input` carries one input
        // frame per output frame at `in_channels` interleave, or is empty (the sanctioned
        // no-input call). A wrong-width capture buffer would otherwise channel-smear silently.
        debug_assert!(
            input.is_empty() || input.len() * ch == out.len() * in_ch,
            "duplex input must carry one frame per output frame at input_channels stride \
             (got {} input samples for {} frames at {} input channels)",
            input.len(),
            out.len() / ch,
            in_ch
        );
        let frames = out.len() / ch;
        // Fresh outbound collection for this fill; the render path appends, the caller drains.
        self.outbound.clear();
        // The no-input path stages nothing: `in_scratch` starts zeroed, so until some call
        // stages real input (`in_dirty`), skipping the staging loop *is* staging zeros — an
        // input-bound patch driven by plain `fill()` pays nothing per frame here. Once dirty,
        // an empty-input call must re-stage zeros so stale samples never re-enter the render.
        let stage = !input.is_empty() || self.in_dirty;
        if !input.is_empty() {
            self.in_dirty = true;
        }
        for f in 0..frames {
            if self.pos >= self.block_size() {
                self.render_next();
                self.pos = 0;
            }
            // Stage this frame's input for the *next* rendered block (see the alignment note
            // above). Missing samples stage zeros, so partial input degrades dark, not stale.
            if stage {
                for c in 0..in_ch {
                    self.in_scratch[c][self.pos] = input.get(f * in_ch + c).copied().unwrap_or(0.0);
                }
            }
            for c in 0..ch {
                out[f * ch + c] = self.scratch[c][self.pos];
            }
            self.pos += 1;
        }
    }

    /// Render one block into `scratch`, consuming any queued Messages and the staged input.
    /// Until real input has ever been staged (`in_dirty`), the render sees **no** input
    /// channels rather than staged zeros: an input pipe with a declared `default` then
    /// materializes that default (no device stream is not the same as a
    /// silent device stream). Once a stream exists, staged zeros are honest device silence.
    fn render_next(&mut self) {
        let msgs = std::mem::take(&mut self.pending);
        let inputs: &[Vec<f32>] = if self.in_dirty { &self.in_scratch } else { &[] };
        self.renderer.render_block_multi(
            &mut self.plan,
            &msgs,
            inputs,
            &mut self.scratch,
            &mut self.outbound,
        );
    }
}
