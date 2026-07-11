//! The RT-side install slot (ADR-0046 §7, ADR-0050 §2): the render-side unit each shell drives
//! **instead of calling [`Engine::fill`] directly**.
//!
//! [`RenderSlot`] owns the live [`Engine`], the install-mailbox consumer ([`RenderMailbox`]), and
//! the master-gain **ramp state**. Per callback it (ADR-0046 §3, ADR-0050 §2):
//!
//! 1. **peeks** the install slot ([`RenderMailbox::has_install`], one atomic load) and, if a swap
//!    is waiting and no ramp is running, begins a **raised-cosine master-gain down-ramp** — it does
//!    *not* consume the bundle yet (ADR-0050 §2: "begin the ramp at the callback top; install when
//!    it reaches zero");
//! 2. renders the current Engine and applies the ramp scalar as **one multiply per output sample**
//!    on the interleaved logical master — the new master-gain machinery ADR-0050 §2 puts here so
//!    both shells (native callback, web worklet) inherit it. At steady state (gain == 1.0) there is
//!    no per-sample multiply at all: the fast path is a bare [`Engine::fill_duplex`];
//! 3. when the down-ramp reaches **zero** it **installs at zero** — drains the bundle, box-transplants
//!    the survivors via [`Engine::transplant_survivors`] (ADR-0046 §4, the blessed `mem::swap`
//!    primitive from #320), swaps the new Engine in, and posts the retiree back through the mailbox
//!    for **off-thread reclaim** — then ramps back up.
//!
//! **Everything on this path is RT-safe** (ADR-0012): no alloc, lock, syscall, or drop on the render
//! thread. The transplant is a bounded pointer-swap loop; the retiree is posted in the **same box**
//! the install arrived in (its allocation is reused, never freed here); the only heap free is the
//! Coordinator's off-thread reclaim of that box. Non-survivors' fresh boxes start cold but are
//! silenced under the ramp (their hard cut lands at master-zero — inaudible, ADR-0050 §4); survivors
//! keep voice/gate state and ring through the up-ramp. The ~15ms hanging-note window (a note-off lost
//! in the discard window) is accepted (ADR-0050 §5).

use crate::message::{Arg, Message};

use super::mailbox::RenderMailbox;
use super::swap::{InstallBundle, RenderSide};
use crate::engine::Engine;

/// Master-gain ramp duration **per edge** (ADR-0050 §3): raised-cosine, nominal 10ms, **fixed and
/// hard-coded** — no document/profile knob, no opt-out. The implementation ticket may tune within
/// 5–20ms without a new decision; this is that one constant. A full swap ducks for ~2× this.
const RAMP_MS_PER_EDGE: f32 = 10.0;

/// Where the master-gain ramp is in its down → install-at-zero → up cycle (ADR-0050 §2).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    /// Gain is a flat 1.0 — no ramp. The fast path: no per-sample multiply, only a mailbox peek.
    Steady,
    /// Fading the master to zero ahead of the install (ADR-0050 §2 fade-down).
    Down,
    /// Fading the master back up after the install-at-zero (ADR-0050 §2 fade-up).
    Up,
}

/// The raised-cosine master-gain ramp (ADR-0050 §2/§3): the "gain stage" that never existed on the
/// bare per-channel master sum. Holds a precomputed half-cosine curve and the running position; the
/// curve is built once at construction (off-thread), so applying the ramp is table lookups + a
/// multiply — allocation-free on the render thread.
struct MasterGainRamp {
    /// The declick curve, `edge + 1` samples: `curve[0] == 1.0` … `curve[edge] == 0.0`, a raised
    /// cosine `0.5·(1 + cos(π·i/edge))`. The down edge reads it forward (1 → 0); the up edge reads
    /// it mirrored (`curve[2·edge − pos]`, 0 → 1). One table serves both edges.
    curve: Vec<f32>,
    /// Samples per edge (`RAMP_MS_PER_EDGE` at the Engine's sample rate), ≥ 1.
    edge: usize,
    phase: Phase,
    /// Position within the full 2·edge cycle: `0..edge` is the down edge, `edge..2·edge` the up.
    pos: usize,
}

impl MasterGainRamp {
    /// Precompute the raised-cosine curve for `sample_rate` (off-thread; the render side only ever
    /// reads it). `edge` is clamped to ≥ 1 so a degenerate sample rate can never divide by zero or
    /// make a zero-length ramp.
    fn new(sample_rate: f32) -> Self {
        let edge = ((RAMP_MS_PER_EDGE / 1000.0) * sample_rate).round() as usize;
        let edge = edge.max(1);
        let mut curve = Vec::with_capacity(edge + 1);
        for i in 0..=edge {
            let t = i as f32 / edge as f32; // 0.0 ..= 1.0
            curve.push(0.5 * (1.0 + (std::f32::consts::PI * t).cos()));
        }
        // Exact endpoints (guard against cos rounding): full open at 0, dead silent at edge.
        curve[0] = 1.0;
        curve[edge] = 0.0;
        Self {
            curve,
            edge,
            phase: Phase::Steady,
            pos: 0,
        }
    }

    #[inline]
    fn is_active(&self) -> bool {
        self.phase != Phase::Steady
    }

    /// Begin a down-ramp from full gain (ADR-0050 §2). Precondition: currently [`Phase::Steady`].
    #[inline]
    fn begin_down(&mut self) {
        self.phase = Phase::Down;
        self.pos = 0;
    }
}

/// The production RT-side install slot (ADR-0046 §7). Built from the [`RenderSide`] a
/// [`Coordinator`](super::swap::Coordinator) hands out, it is what the shell's audio callback drives
/// each block. See the module docs for the per-callback contract.
pub struct RenderSlot {
    engine: Engine,
    mailbox: RenderMailbox<InstallBundle>,
    ramp: MasterGainRamp,
    /// A retiree that [`RenderMailbox::post_retiree`] refused (the retire slot was occupied). The
    /// one-in-flight discipline (ADR-0046 §2) makes this **unreachable**, but if it ever happened we
    /// must not *drop* the box on the render thread (an RT free). Stash it and re-post at the top of
    /// the next callback; if the slot never re-opens it rides here until the slot is dropped
    /// off-thread. This is the only reason the render thread never frees an [`InstallBundle`].
    stranded_retiree: Option<Box<InstallBundle>>,
}

impl RenderSlot {
    /// Adopt the [`RenderSide`] (initial Engine + render mailbox) a Coordinator handed out, sizing
    /// the ramp to the Engine's sample rate. All allocation (the curve table) happens here, at
    /// setup, off the audio thread.
    pub fn new(side: RenderSide) -> Self {
        let ramp = MasterGainRamp::new(side.engine.sample_rate());
        Self {
            engine: side.engine,
            mailbox: side.mailbox,
            ramp,
            stranded_retiree: None,
        }
    }

    /// Logical master channel count (ADR-0026) — [`fill`](Self::fill) interleaves this many.
    pub fn channels(&self) -> usize {
        self.engine.channels()
    }

    /// Logical input channel count (ADR-0038 §3) — [`fill_duplex`](Self::fill_duplex) de-interleaves
    /// this many.
    pub fn input_channels(&self) -> usize {
        self.engine.input_channels()
    }

    /// The core block size this slot's Engine renders in.
    pub fn block_size(&self) -> usize {
        self.engine.block_size()
    }

    /// The sample rate this slot's Engine was instantiated for.
    pub fn sample_rate(&self) -> f32 {
        self.engine.sample_rate()
    }

    /// Queue an inbound external message in flat primitive form (ADR-0030) on the live Engine — the
    /// slot forwards it, exactly as a shell would to a bare Engine.
    pub fn queue_osc(&mut self, address: &str, args: &[Arg]) {
        self.engine.queue_osc(address, args);
    }

    /// Queue a typed [`Message`] on the live Engine.
    pub fn queue(&mut self, msg: Message) {
        self.engine.queue(msg);
    }

    /// Drain the outbound Messages the most recent fill produced (ADR-0026), in emission order.
    pub fn drain_outbound(&mut self) -> std::vec::Drain<'_, Message> {
        self.engine.drain_outbound()
    }

    /// Whether a master-gain ramp is currently in flight (down or up). Introspection for the
    /// shells and tests — steady state is `false`.
    pub fn is_ramping(&self) -> bool {
        self.ramp.is_active()
    }

    /// Samples per ramp edge (ADR-0050 §3, the fixed ~10ms). A full swap ducks over `2 ×` this many
    /// frames, with the master hitting exactly zero at frame `ramp_edge_frames()` of the ramp.
    pub fn ramp_edge_frames(&self) -> usize {
        self.ramp.edge
    }

    /// Fill `out` with interleaved logical samples (the no-input convenience for
    /// [`fill_duplex`](Self::fill_duplex)). Bound input pipes fall back to their declared defaults.
    pub fn fill(&mut self, out: &mut [f32]) {
        self.fill_duplex(&[], out);
    }

    /// The per-callback contract (module docs): peek → (ramp) → install-at-zero → (ramp) → render.
    ///
    /// `input` is the interleaved logical input master (ADR-0038 §3), one input frame per output
    /// frame, or empty for the no-input path — identical to [`Engine::fill_duplex`].
    pub fn fill_duplex(&mut self, input: &[f32], out: &mut [f32]) {
        // Re-post a stranded retiree if the one-in-flight invariant was ever (impossibly) violated.
        // A no-op on every real callback; here so the render thread never has to drop the box.
        self.reflush_stranded_retiree();

        // Begin a down-ramp if idle and a swap is waiting. Peek (a load), do not drain: the bundle
        // stays in the slot until the ramp reaches zero (ADR-0050 §2). Only the render side drains
        // the install slot and one-swap-in-flight (ADR-0046 §2) keeps the Coordinator from replacing
        // it mid-ramp, so what we peek here is exactly what we drain at zero.
        if !self.ramp.is_active() && self.mailbox.has_install() {
            self.ramp.begin_down();
        }

        // Fast path (ADR-0050 §2 "nothing at steady state"): no ramp ⇒ a bare Engine fill, zero
        // per-sample multiplies. The only steady-state cost over calling the Engine directly is the
        // one `has_install` load above.
        if !self.ramp.is_active() {
            self.engine.fill_duplex(input, out);
            return;
        }

        self.fill_ramping(input, out);
    }

    /// The ramp path: render the buffer in phase-bounded segments (one Engine per segment), applying
    /// the raised-cosine gain per frame, installing at the zero crossing. Split because the install
    /// (Engine swap) must land at the exact frame the master is silent, which may fall mid-buffer.
    fn fill_ramping(&mut self, input: &[f32], out: &mut [f32]) {
        let ch = self.engine.channels();
        let in_ch = self.engine.input_channels();
        let frames = out.len() / ch.max(1);
        let edge = self.ramp.edge;

        let mut f = 0;
        while f < frames {
            match self.ramp.phase {
                Phase::Down => {
                    // Down edge runs positions `pos..edge`; cap at the buffer end.
                    let seg = (frames - f).min(edge - self.ramp.pos);
                    render_segment(&mut self.engine, input, out, in_ch, ch, f, seg);
                    for k in 0..seg {
                        let g = self.ramp.curve[self.ramp.pos + k];
                        scale_frame(out, ch, f + k, g);
                    }
                    self.ramp.pos += seg;
                    f += seg;
                    if self.ramp.pos == edge {
                        // Master is at (heading to) zero: install now, then ramp up.
                        self.install_at_zero();
                        self.ramp.phase = Phase::Up;
                    }
                }
                Phase::Up => {
                    // Up edge runs positions `pos..2·edge`; gain mirrors the curve (0 → 1).
                    let seg = (frames - f).min(2 * edge - self.ramp.pos);
                    render_segment(&mut self.engine, input, out, in_ch, ch, f, seg);
                    for k in 0..seg {
                        let g = self.ramp.curve[2 * edge - (self.ramp.pos + k)];
                        scale_frame(out, ch, f + k, g);
                    }
                    self.ramp.pos += seg;
                    f += seg;
                    if self.ramp.pos == 2 * edge {
                        self.ramp.phase = Phase::Steady;
                        self.ramp.pos = 0;
                    }
                }
                // The ramp finished mid-buffer: render the remainder at full gain (no multiply).
                Phase::Steady => {
                    render_segment(&mut self.engine, input, out, in_ch, ch, f, frames - f);
                    f = frames;
                }
            }
        }
    }

    /// Install at master-zero (ADR-0046 §3/§4, ADR-0050 §2): drain the bundle, box-transplant the
    /// survivors from the current Engine into the fresh one, swap the fresh Engine live, and post the
    /// retiree back **in the same box** for off-thread reclaim. All pointer swaps — no alloc, no
    /// drop, no lock on the render thread.
    fn install_at_zero(&mut self) {
        let Some(mut bundle) = self.mailbox.take_install() else {
            // Unreachable: `has_install` was true when the ramp began and only the render side drains
            // the slot, so the bundle is still here. If it somehow isn't, ducking with no swap (ramp
            // back up on the current Engine) is a safe degradation — a click-free no-op.
            return;
        };
        // Move the survivors' live boxes into the fresh Engine; the fresh Engine's cold boxes for
        // those nodes land back in `bundle.engine`, to retire off-thread (ADR-0046 §4).
        bundle
            .engine
            .transplant_survivors(&mut self.engine, bundle.migration.survivors());
        // Swap the fresh Engine live; `bundle.engine` now holds the retiring one. Reusing the box's
        // storage (rather than `Box::new`) is what keeps the post allocation-free.
        std::mem::swap(&mut self.engine, &mut bundle.engine);
        if let Err(returned) = self.mailbox.post_retiree(bundle) {
            // Unreachable under one-in-flight (ADR-0046 §2): the retire slot is vacant here. Never
            // drop on the render thread — stash and retry next callback.
            self.stranded_retiree = Some(returned);
        }
    }

    /// Retry a stranded retiree post (see [`Self::stranded_retiree`]). RT-safe: an `Option::take`
    /// plus at most one atomic compare-exchange, no alloc/free/drop.
    #[inline]
    fn reflush_stranded_retiree(&mut self) {
        if let Some(retiree) = self.stranded_retiree.take() {
            if let Err(returned) = self.mailbox.post_retiree(retiree) {
                self.stranded_retiree = Some(returned);
            }
        }
    }
}

/// Render `seg` frames of `input`/`out` starting at frame `f` through `engine`, slicing input and
/// output to the segment. Chunk-size independence (proven for [`Engine::fill_duplex`]) makes a
/// segmented render identical, sample-for-sample, to one whole-buffer render — so splitting at the
/// install point costs nothing but a second `fill` call.
#[inline]
fn render_segment(
    engine: &mut Engine,
    input: &[f32],
    out: &mut [f32],
    in_ch: usize,
    ch: usize,
    f: usize,
    seg: usize,
) {
    let out_sub = &mut out[f * ch..(f + seg) * ch];
    if input.is_empty() {
        engine.fill_duplex(&[], out_sub);
    } else {
        engine.fill_duplex(&input[f * in_ch..(f + seg) * in_ch], out_sub);
    }
}

/// Multiply every channel of frame `f` (interleaved at `ch`) by the master gain `g` — the one
/// multiply per output sample ADR-0050 §2 spends while ramping.
#[inline]
fn scale_frame(out: &mut [f32], ch: usize, f: usize, g: f32) {
    let base = f * ch;
    for c in 0..ch {
        out[base + c] *= g;
    }
}
