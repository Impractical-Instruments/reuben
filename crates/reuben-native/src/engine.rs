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
    /// Logical **input** channel count (ADR-0038 §3), fixed for this Plan; `0` for a patch
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

    /// The core block size this engine renders in.
    pub fn block_size(&self) -> usize {
        self.plan.config.block_size
    }

    /// Logical master channel count (ADR-0026). `fill` interleaves this many channels.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Logical **input** channel count (ADR-0038 §3). [`Engine::fill_duplex`] de-interleaves
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

    /// Queue an inbound OSC datagram (ADR-0030): convert its flat primitive args into the single
    /// typed [`Message`] the destination port carries (driven by the port's Arg type), then queue
    /// it. Dropped silently if the address routes to no node/port or the args don't fit — an
    /// authoring error the boundary already tolerates. The conversion needs the Plan, which the
    /// engine owns, so it lives here rather than in the address-blind OSC decode layer.
    pub fn queue_osc(&mut self, osc: &crate::osc::OscIn) {
        if let Some(msg) = self.plan.osc_in_message(&osc.address, &osc.args) {
            self.pending.push(msg);
        }
    }

    /// Drain the outbound Messages produced by the most recent [`Engine::fill`] (ADR-0026), in
    /// emission order. The caller (native's OSC-out path) encodes and UDP-sends them. Empty unless
    /// the instrument has an `osc_out` sink that fired; call right after `fill`, before the next.
    pub fn drain_outbound(&mut self) -> std::vec::Drain<'_, Message> {
        self.outbound.drain(..)
    }

    /// Fill `out` with **interleaved logical** samples, rendering core blocks as needed.
    /// `out.len()` must be a multiple of [`Engine::channels`]; frame `f`, channel `c` lands at
    /// `out[f * channels + c]`. The no-input convenience: an instrument's bound input pipes
    /// fall back to their declared defaults (a bare pipe reads silence) — use
    /// [`Engine::fill_duplex`] to supply the logical input.
    pub fn fill(&mut self, out: &mut [f32]) {
        self.fill_duplex(&[], out);
    }

    /// [`Engine::fill`] with the **logical input master** supplied (ADR-0038 §3): `input` is
    /// interleaved at [`Engine::input_channels`] channels and carries **one input frame per
    /// output frame** (`input.len() / input_channels == out.len() / channels`; same clock —
    /// the device layer resamples before this seam, ADR-0038 §8). A short `input` stages
    /// zeros for the missing samples — dark-degrade (§7). A caller with no input stream can
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
    /// materializes that default (ADR-0038 §3/§7 — no device stream is not the same as a
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::osc::OscIn;
    use crate::rigs::default_rig;
    use reuben_core::message::Arg;
    use reuben_core::AudioConfig;

    fn engine_with_note() -> Engine {
        let cfg = AudioConfig::new(48_000.0, 256);
        let plan = Plan::instantiate(default_rig(), cfg).expect("instantiate");
        let mut e = Engine::new(plan);
        // Drive a note in through the real inbound boundary: flat OSC args -> typed `Arg::Note`,
        // driven by the voicer's note port type (ADR-0030).
        e.queue_osc(&OscIn {
            address: "/voicer/notes".into(),
            args: vec![Arg::F32(69.0), Arg::F32(1.0)],
        });
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
        // route, stamped with the node address (ADR-0026). The sink's input is the type-agnostic
        // pass-through (issue #141): a single primitive atom crosses the inbound boundary
        // verbatim and echoes out unchanged — the OSC loopback path.
        let mut e = Engine::new(osc_out_plan());
        e.queue_osc(&OscIn {
            address: "/fb/in".into(),
            args: vec![Arg::F32(0.5)],
        });
        let mut out = vec![0.0f32; e.block_size() * e.channels()];
        e.fill(&mut out);

        let drained: Vec<_> = e.drain_outbound().collect();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].address, "/fb");
        assert_eq!(drained[0].arg, Arg::F32(0.5));
        // Drained once: the next fill (no input) yields nothing.
        e.fill(&mut out);
        assert_eq!(e.drain_outbound().count(), 0);
    }

    /// A one-pipe passthrough bound to logical input channel 0 (ADR-0038 §3, P3).
    fn input_engine(block_size: usize) -> Engine {
        const MIC: &str = r#"{
          "format_version": 2,
          "instrument": "mic_through",
          "interface": {
            "inputs":  { "mic": { "type": "f32_buffer", "channel": 0 } },
            "outputs": { "main": { "from": "/out.audio" } }
          },
          "nodes": [
            { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/mic" } } }
          ]
        }"#;
        let graph = reuben_core::load(MIC, &reuben_core::Registry::builtin()).expect("load");
        let plan =
            Plan::instantiate(graph, AudioConfig::new(48_000.0, block_size)).expect("instantiate");
        Engine::new(plan)
    }

    /// Deterministic mono input as a pure function of the global frame index.
    fn in_sig(global_frame: usize) -> f32 {
        (global_frame % 89) as f32 / 89.0 - 0.5
    }

    #[test]
    fn fill_duplex_stages_input_one_block_ahead() {
        // The alignment pin (see `fill_duplex` docs): the block rendered at global frame k*B
        // consumes input frames [(k-1)*B, k*B), so a bound pipe echoes the input with exactly
        // one core block of latency — and the first block is silence, whatever the input.
        let mut e = input_engine(128);
        assert_eq!(e.input_channels(), 1);
        let b = e.block_size();
        let ch = e.channels();
        let frames = 3 * b;
        let input: Vec<f32> = (0..frames).map(in_sig).collect(); // 1 input channel
        let mut out = vec![0.0f32; frames * ch];
        e.fill_duplex(&input, &mut out);
        for f in 0..frames {
            let expect = if f < b { 0.0 } else { in_sig(f - b) };
            assert_eq!(
                out[f * ch].to_bits(),
                expect.to_bits(),
                "frame {f}: input must land exactly one block later"
            );
        }
    }

    #[test]
    fn fill_duplex_is_independent_of_chunk_size() {
        // The input-side analog of `fill_is_independent_of_chunk_size`: one big duplex fill
        // must equal many ragged ones sample-for-sample — output stays a pure function of the
        // global frame index however input+output arrive.
        let probe = input_engine(128);
        let ch = probe.channels();
        let in_ch = probe.input_channels();
        let total_frames = 5_000;

        let input: Vec<f32> = (0..total_frames * in_ch)
            .map(|i| in_sig(i / in_ch))
            .collect();

        let mut whole = input_engine(128);
        let mut a = vec![0.0f32; total_frames * ch];
        whole.fill_duplex(&input, &mut a);

        let mut chunked = input_engine(128);
        let mut b = vec![0.0f32; total_frames * ch];
        let mut f = 0;
        for step in [37usize, 256, 1, 500, 129].iter().cycle() {
            if f >= total_frames {
                break;
            }
            let end = (f + step).min(total_frames);
            chunked.fill_duplex(&input[f * in_ch..end * in_ch], &mut b[f * ch..end * ch]);
            f = end;
        }

        for (k, (x, y)) in a.iter().zip(&b).enumerate() {
            assert_eq!(x.to_bits(), y.to_bits(), "mismatch at sample {k}");
        }
    }

    #[test]
    fn fill_without_input_is_silence_for_a_bound_patch() {
        // The no-input convenience on an input-bound patch reads zeros (ADR-0038 §7) — the
        // exact behavior every pre-P5 caller (no input stream yet) gets.
        let mut e = input_engine(256);
        let mut out = vec![1.0f32; 2048 * e.channels()];
        e.fill(&mut out);
        assert!(
            out.iter().all(|&s| s == 0.0),
            "unfed input pipe must be silent"
        );
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
