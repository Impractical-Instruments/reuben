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
//! RT-safety: queueing resolves the address against the Engine's Plan once, so the `pending` queue
//! holds each message's target index and payload, never its address. Queue + render of flat-arg
//! control does no heap work: the queue is preallocated and cleared, never replaced (a burst past
//! its high-water mark grows it once), and nothing in a queued entry owns an allocation unless its
//! payload does, and that payload goes to the retire queue ([`Engine::set_retire_queue`]) rather
//! than being dropped here. [`Engine::queue`] takes an already-built [`Message`] and drops its
//! address there.
//!
//! The queue belongs to this Engine and indexes this Engine's Plan. An install replaces the whole
//! Engine, so control still pending at that moment leaves with the retiring Engine and is never
//! applied — to it or to its successor.

use alloc::vec;
use alloc::vec::Vec;

use crate::coordinator::retire::{retire_to, RetireQueue};
use crate::message::{Arg, Message};
use crate::plan::Plan;
use crate::render::{Renderer, Resolved};

/// Preallocated capacity of the inbound queue. Sized well past a block's worth of live control
/// traffic (a played chord plus a few knob moves), so steady state never grows it on the audio
/// thread.
const PENDING_CAP: usize = 64;

/// Owns a Plan + Renderer and serves **interleaved logical** audio one block at a time into
/// arbitrary buffers. "Logical" = the instrument's master channels; mapping those
/// onto the real device's channel count is the shell's job (native's `audio.rs`), not the
/// engine's.
pub struct Engine {
    plan: Plan,
    renderer: Renderer,
    /// Control to apply at the start of the next rendered block, resolved against `plan`.
    pending: Vec<Resolved>,
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
            pending: Vec::with_capacity(PENDING_CAP),
            outbound: Vec::new(),
            channels,
            in_channels,
            scratch: vec![vec![0.0; block_size]; channels],
            in_scratch: vec![vec![0.0; block_size]; in_channels],
            in_dirty: false,
            pos: block_size, // exhausted -> first fill renders immediately
        }
    }

    /// Retire every heap-owning value this Engine's render thread lets go of into `queue`, for
    /// the queue's drainer to drop. Call off the render thread, before the Engine is installed.
    /// Without one, those values are dropped on the render thread where they are replaced.
    pub fn set_retire_queue(&mut self, queue: RetireQueue) {
        self.renderer.set_retire_queue(queue);
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

    /// Queue a Message to apply at the start of the next rendered block. Its address is resolved
    /// here and the Message's address dropped; an address that reaches no port is dropped whole,
    /// as [`Engine::queue_osc`] drops one. The payload is not checked against the port until the
    /// block routes it.
    pub fn queue(&mut self, msg: Message) {
        if let Some((target, _)) = self.plan.resolve(&msg.address) {
            self.pending.push(Resolved {
                target,
                frame: msg.frame,
                arg: msg.arg,
            });
        }
    }

    /// Queue an inbound external message in **flat primitive form**: an address plus
    /// the flat `F32`/`I32`/`Str` args, however the shell decoded them (a UDP/OSC datagram, a
    /// worklet control buffer). Resolves the address to its destination port once and converts
    /// the args into the single typed [`Arg`] that port carries, then queues the pair. Dropped
    /// silently if the address routes to no node/port or the args don't fit — an authoring error
    /// the boundary already tolerates. The conversion needs the Plan, which the engine owns, so it
    /// lives here rather than in the address-blind decode layers.
    pub fn queue_osc(&mut self, address: &str, args: &[Arg]) {
        if let Some((target, arg)) = self.plan.osc_in(address, args) {
            self.pending.push(Resolved {
                target,
                frame: 0,
                arg,
            });
        }
    }

    /// Drain the outbound Messages produced by the most recent [`Engine::fill`], in
    /// emission order. The caller (native's OSC-out path) encodes and UDP-sends them. Empty unless
    /// the instrument has an `osc_out` sink that fired; call right after `fill`, before the next.
    pub fn drain_outbound(&mut self) -> alloc::vec::Drain<'_, Message> {
        self.outbound.drain(..)
    }

    /// Transplant survivor operator boxes from a `retiring` Engine into this (freshly built) one,
    /// per the Coordinator's precomputed survivor pairs — the `(old, new)` slice a
    /// [`MigrationTable`](crate::coordinator::MigrationTable) yields via `survivors()`.
    /// Engine owns two Plans here (fresh + retiring) but no longer indexes their nodes: it forwards
    /// to the pointer-swap primitive that lives on [`Plan`] (which owns its nodes). See
    /// `Plan::transplant_survivors` for the box-move semantics, the pairing invariant, and the
    /// RT-safety guarantee (a bounded `mem::swap` loop plus the hosted-probe exchange — no
    /// allocation, no drop that frees, no lock). Runs at the render-side install slot's callback
    /// top.
    pub fn transplant_survivors(&mut self, retiring: &mut Engine, pairs: &[(usize, usize)]) {
        self.plan.transplant_survivors(&mut retiring.plan, pairs);
    }

    /// Carry live input values from a `retiring` Engine into this (freshly built) one. Each place
    /// may appear at most once per side; see `Plan::carry_live_values`.
    pub fn carry_live_values(&mut self, retiring: &mut Engine, carries: &[crate::plan::LiveCarry]) {
        self.plan.carry_live_values(&mut retiring.plan, carries);
    }

    /// Take the count of inbound messages dropped for addressing an input a wire or an interface
    /// pipe feeds, leaving zero. RT-safe: a field read and write.
    pub fn take_dropped_wired_writes(&mut self) -> u64 {
        self.plan.take_dropped_wired_writes()
    }

    /// Add `n` to the count [`take_dropped_wired_writes`](Self::take_dropped_wired_writes)
    /// returns: an install credits the fresh Engine with what the retiring one had not handed on.
    pub fn credit_dropped_wired_writes(&mut self, n: u64) {
        self.plan.dropped_wired_writes += n;
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
                for (c, staged) in self.in_scratch.iter_mut().enumerate() {
                    staged[self.pos] = input.get(f * in_ch + c).copied().unwrap_or(0.0);
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
        let inputs: &[Vec<f32>] = if self.in_dirty { &self.in_scratch } else { &[] };
        self.renderer.render_block_resolved(
            &mut self.plan,
            &self.pending,
            inputs,
            &mut self.scratch,
            &mut self.outbound,
        );
        // `drain`, not `mem::take`: the queue keeps its capacity for the next block. A queued
        // string can be the last reference once its port has latched or refused it.
        let retire = self.renderer.retire_queue();
        for entry in self.pending.drain(..) {
            retire_to(retire, entry.arg);
        }
    }
}
