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
//! Engine — usually via [`Engine::from_document`] — then drives `queue_osc` → `fill` /
//! `fill_duplex` → `drain_outbound`. Protocol decode (UDP/OSC datagrams, worklet message
//! buffers) stays in the shells; the Engine takes the already-flat primitive args.
//!
//! NOTE (RT-debt): [`Renderer::render_block`] is allocation-free, but [`Engine::fill`]'s
//! message handoff (the `pending` Vec) still churns the heap when messages flow, so the
//! audio callback isn't fully allocation-free yet. A lock-free, preallocated handoff is
//! tracked for later.

use crate::config::AudioConfig;
use crate::format::{load_instrument, LoadError, LoadWarning};
use crate::message::{Arg, Message};
use crate::plan::{Plan, PlanError};
use crate::registry::Registry;
use crate::render::Renderer;
use crate::resources::ResourceResolver;

/// Why [`Engine::from_document`] failed: the document didn't load, or the loaded graph
/// didn't instantiate.
#[derive(Debug)]
pub enum FromDocumentError {
    /// The instrument document failed to parse/build (see [`LoadError`]).
    Load(LoadError),
    /// The loaded graph failed to instantiate into a [`Plan`] (see [`PlanError`]).
    Plan(PlanError),
}

impl std::fmt::Display for FromDocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FromDocumentError::Load(e) => write!(f, "load instrument: {e}"),
            // PlanError has no Display of its own; its Debug form is the diagnostic.
            FromDocumentError::Plan(e) => write!(f, "instantiate plan: {e:?}"),
        }
    }
}

impl std::error::Error for FromDocumentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            FromDocumentError::Load(e) => Some(e),
            FromDocumentError::Plan(_) => None,
        }
    }
}

/// Owns a Plan + Renderer and serves **interleaved logical** audio one block at a time into
/// arbitrary buffers. "Logical" = the instrument's master channels (ADR-0026); mapping those
/// onto the real device's channel count is the shell's job (native's `audio.rs`), not the
/// engine's.
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

    /// Construct an engine straight from an instrument document: `load_instrument` →
    /// [`Plan::instantiate`] → [`Engine::new`]. The one place this glue is written — every
    /// shell (native, web, game) calls it instead of re-wiring the chain. Returns the load
    /// warnings alongside the engine: resource problems are non-fatal (ADR-0016) but they are
    /// authoring errors the shell must surface, not swallow.
    pub fn from_document(
        text: &str,
        registry: &Registry,
        resolver: &dyn ResourceResolver,
        config: AudioConfig,
    ) -> Result<(Self, Vec<LoadWarning>), FromDocumentError> {
        let loaded = load_instrument(text, registry, resolver).map_err(FromDocumentError::Load)?;
        let plan = Plan::instantiate(loaded.graph, config).map_err(FromDocumentError::Plan)?;
        Ok((Self::new(plan), loaded.warnings))
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

    /// Queue an inbound external message in **flat primitive form** (ADR-0030): an address plus
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

    /// Drain the outbound Messages produced by the most recent [`Engine::fill`] (ADR-0026), in
    /// emission order. The caller (native's OSC-out path) encodes and UDP-sends them. Empty unless
    /// the instrument has an `osc_out` sink that fired; call right after `fill`, before the next.
    pub fn drain_outbound(&mut self) -> std::vec::Drain<'_, Message> {
        self.outbound.drain(..)
    }

    /// Transplant survivor operator boxes from a `retiring` Engine into this (freshly built) one,
    /// per a precomputed migration table (ADR-0046 §4). Each `(old_index, new_index)` pair moves
    /// the surviving box — its operator instance *is* its state (ADR-0046 §4), including a voicer's
    /// hosted voice sub-plans — from `retiring.nodes[old_index]` into `self.nodes[new_index]`; the
    /// displaced cold box lands in `retiring` and frees off-thread with it. The new Plan's wiring
    /// and latches (which live in the PlanNode, not the box) stay this Engine's, so a survivor
    /// re-reads its inputs from the *new* document (ADR-0045 §2). The survivor key
    /// ([`crate::coordinator::manifest`]) guarantees each pair shares operator type + instantiate-
    /// time identity, so the transplanted box's internal layout matches its new Plan node.
    ///
    /// **RT-safe:** a bounded loop of [`std::mem::swap`] over `Vec<Box<dyn Operator>>` — pointer
    /// swaps only, no allocation, no drop, no lock. This is the transplant the render-side install
    /// slot (ticket #321) runs at the callback top; it is exposed here because the primitive
    /// touches Engine internals, while the migration *table* (which pairs, computed how) is owned
    /// by the Coordinator side.
    pub fn transplant_survivors(&mut self, retiring: &mut Engine, pairs: &[(usize, usize)]) {
        for &(old_index, new_index) in pairs {
            debug_assert!(
                old_index < retiring.plan.nodes.len() && new_index < self.plan.nodes.len(),
                "migration table index out of range — mispaired table/engine"
            );
            std::mem::swap(
                &mut retiring.plan.nodes[old_index].ops,
                &mut self.plan.nodes[new_index].ops,
            );
        }
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
    use crate::resources::MemoryResolver;

    /// The default rig, loaded as data exactly like native's embedded copy (its voice
    /// sub-patch resolved in-memory) — the engine tests drive the real document path.
    const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");
    const DEFAULT_VOICE_JSON: &str = include_str!("../../../instruments/voices/default-voice.json");

    fn default_resolver() -> MemoryResolver {
        let mut r = MemoryResolver::new();
        r.insert_text("voices/default-voice.json", DEFAULT_VOICE_JSON);
        r
    }

    fn default_engine(cfg: AudioConfig) -> Engine {
        let (engine, warnings) =
            Engine::from_document(DEFAULT_JSON, &Registry::builtin(), &default_resolver(), cfg)
                .expect("default.json is a valid instrument");
        assert!(warnings.is_empty(), "unexpected warnings: {warnings:?}");
        engine
    }

    fn engine_with_note() -> Engine {
        let mut e = default_engine(AudioConfig::new(48_000.0, 256));
        // Drive a note in through the real inbound boundary: flat OSC args -> typed `Arg::Note`,
        // driven by the voicer's note port type (ADR-0030).
        e.queue_osc("/voicer/notes", &[Arg::F32(69.0), Arg::F32(1.0)]);
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
    fn from_document_surfaces_load_and_plan_errors() {
        let cfg = AudioConfig::new(48_000.0, 256);
        let result = Engine::from_document(
            "{ not json",
            &Registry::builtin(),
            &MemoryResolver::new(),
            cfg,
        );
        match result {
            Err(FromDocumentError::Load(_)) => {}
            Err(other) => panic!("expected a load error, got {other:?}"),
            Ok(_) => panic!("malformed document must not construct"),
        }

        // The Plan arm: a document that loads fine but whose graph cannot instantiate (a
        // wire cycle with no explicit unit-delay).
        const CYCLIC: &str = r#"{
          "format_version": 2,
          "instrument": "cycle",
          "nodes": [
            { "type": "add_f32_signal", "address": "/a", "inputs": { "a": { "from": "/b" } } },
            { "type": "add_f32_signal", "address": "/b", "inputs": { "a": { "from": "/a" } } },
            { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/a" } } }
          ]
        }"#;
        let result =
            Engine::from_document(CYCLIC, &Registry::builtin(), &MemoryResolver::new(), cfg);
        match result {
            Err(FromDocumentError::Plan(_)) => {}
            Err(other) => panic!("expected a plan error, got {other:?}"),
            Ok(_) => panic!("cyclic graph must not instantiate"),
        }
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
        use crate::graph::Graph;
        use crate::operators::{OscOut, Oscillator, Output};
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
        e.queue_osc("/fb/in", &[Arg::F32(0.5)]);
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

    #[test]
    fn outbound_string_echoes_intact_after_fill() {
        // The first externally-admitted string on the engine path (issues #206/#207): a single
        // `Str` atom queued at the inbound boundary crosses verbatim, routes through the sink's
        // pass-through input, and drains outbound with the string intact and the sink's node
        // address stamped — the end-to-end string loopback.
        let mut e = Engine::new(osc_out_plan());
        e.queue_osc("/fb/in", &[Arg::Str("hello".into())]);
        let mut out = vec![0.0f32; e.block_size() * e.channels()];
        e.fill(&mut out);

        let drained: Vec<_> = e.drain_outbound().collect();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].address, "/fb");
        assert_eq!(drained[0].arg, Arg::Str("hello".into()));
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
        let graph = crate::load(MIC, &Registry::builtin()).expect("load");
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
    fn fill_duplex_then_empty_input_stages_silence_not_stale_samples() {
        // The `in_dirty` stale-input pin (see `fill_duplex` docs): once real input has been
        // staged, a later empty-input call (`fill()`) must re-stage zeros so the last block
        // of live input never re-enters the render — the obvious refactor (early-return past
        // the staging loop when `input.is_empty()`, exactly what the pre-dirty path does)
        // would loop the final block of mic audio forever.
        let mut e = input_engine(128);
        let b = e.block_size();
        let ch = e.channels();
        // Stage two blocks of real input. One-block-ahead alignment: output block 0 is
        // silence, block 1 echoes input block 0 — and `in_scratch` now holds input block 1,
        // i.e. in_sig(b..2b), staged but not yet rendered.
        let input: Vec<f32> = (0..2 * b).map(in_sig).collect();
        let mut out = vec![0.0f32; 2 * b * ch];
        e.fill_duplex(&input, &mut out);

        // Switch to the no-input path. The first post-switch block legitimately echoes the
        // last real input block — that's the one-block latency playing out what was already
        // staged, not staleness.
        let mut post = vec![9.0f32; b * ch];
        e.fill(&mut post);
        for f in 0..b {
            assert_eq!(
                post[f * ch].to_bits(),
                in_sig(b + f).to_bits(),
                "frame {f}: the already-staged input block must still play out"
            );
        }
        // The second post-switch block reads what `fill()` staged: exact zeros — NOT a
        // replay of `in_scratch`'s previous (real-input) contents.
        e.fill(&mut post);
        for f in 0..b {
            assert_eq!(
                post[f * ch].to_bits(),
                0.0f32.to_bits(),
                "frame {f}: empty input after real input must stage silence, not stale samples"
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
