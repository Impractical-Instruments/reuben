//! The web shell's state machine — everything the C-ABI bridge does, as plain testable Rust.
//!
//! [`WebShell`] owns the staged resources ([`WebResolver`]), the pending instrument document,
//! the constructed [`Engine`], and the fixed planar I/O buffers one worklet quantum reads and
//! writes. The `#[no_mangle]` shims in [`crate::bridge`] are one-line calls into this type, so
//! `cargo test` on the host exercises the real logic (issue #224: only the ABI shims are
//! wasm-gated).
//!
//! Lifecycle (both the main-thread discovery instance and the worklet instance run it):
//! `set_document` → stage/construct rounds — [`WebShell::construct`] returns
//! [`ConstructStatus::Misses`] until every transitively-referenced resource is staged
//! (fetch-on-miss: the resolver records what the load asked for and didn't have) — then
//! `Ready`, and the render loop begins: optional input staging → [`WebShell::render`] →
//! read the planar output. Toy switching = [`WebShell::destroy`] (drop the Engine, drop the
//! staged bundle) and run the lifecycle again; the construct-on-audio-thread gap is accepted
//! (issue #224).

use reuben_core::engine::{Engine, FromDocumentError};
use reuben_core::{AudioConfig, Registry};

use crate::codec::decode_control;
use crate::decode::decode_wav_bytes;
use crate::resolver::{Miss, WebResolver};

/// One engine block per worklet render quantum — Web Audio fixes the quantum at 128 frames,
/// so the drain logic in [`Engine::fill`] is exercised trivially (no bespoke adapter).
pub const BLOCK: usize = 128;

/// Logical output channels the shell carries. The worklet node is stereo
/// (`outputChannelCount: [2]`); an instrument wanting more logical channels (`stereo-sub`)
/// is out of scope for P2 and refused at construct.
pub const MAX_CHANNELS: usize = 2;

/// Logical input channels the shell carries (`mic-space` binds one; stereo capture fits).
pub const MAX_INPUT_CHANNELS: usize = 2;

/// What [`WebShell::construct`] concluded, mapped by the bridge onto the C-ABI's `i32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstructStatus {
    /// The Engine is live; [`WebShell::render`] may run. ABI `0`.
    Ready = 0,
    /// Construction failed; [`WebShell::error`] says why. ABI `1`.
    Failed = 1,
    /// The load wanted resources that aren't staged; read [`WebShell::misses`], fetch,
    /// stage, and construct again. ABI `2`.
    Misses = 2,
}

/// The whole web shell: staged bundle + document + Engine + the quantum I/O buffers.
///
/// The buffers are fixed-size inline arrays so that, when the bridge holds a `WebShell` in a
/// `static`, their linear-memory offsets never move (the P1 finding: the host re-wraps a
/// `Float32Array` view each quantum because memory *growth* detaches views, but it fetches
/// each pointer once).
pub struct WebShell {
    resolver: WebResolver,
    /// The top-level instrument document text, staged before construct.
    document: Option<String>,
    engine: Option<Engine>,
    /// Snapshot of the misses recorded by the last [`WebShell::construct`] attempt.
    misses: Vec<Miss>,
    /// Why the last operation failed — the bridge exposes it via `error_ptr`/`error_len`.
    error: String,
    /// One rendered quantum, planar: `out[ch * BLOCK + f]`, [`MAX_CHANNELS`] wide.
    out: [f32; MAX_CHANNELS * BLOCK],
    /// One quantum of staged input, planar: `input[ch * BLOCK + f]` — the worklet writes
    /// `inputs[0]` here before calling render with `has_input`.
    input: [f32; MAX_INPUT_CHANNELS * BLOCK],
    /// Interleave scratch for [`Engine::fill`] (which speaks interleaved logical frames).
    scratch_out: [f32; MAX_CHANNELS * BLOCK],
    scratch_in: [f32; MAX_INPUT_CHANNELS * BLOCK],
}

impl Default for WebShell {
    fn default() -> Self {
        Self::new()
    }
}

impl WebShell {
    pub fn new() -> Self {
        Self {
            resolver: WebResolver::new(),
            document: None,
            engine: None,
            misses: Vec::new(),
            error: String::new(),
            out: [0.0; MAX_CHANNELS * BLOCK],
            input: [0.0; MAX_INPUT_CHANNELS * BLOCK],
            scratch_out: [0.0; MAX_CHANNELS * BLOCK],
            scratch_in: [0.0; MAX_INPUT_CHANNELS * BLOCK],
        }
    }

    /// Stage the top-level instrument document (JSON text) for the next construct.
    pub fn set_document(&mut self, text: impl Into<String>) {
        self.error.clear();
        self.document = Some(text.into());
    }

    /// Stage a text resource (voice/patch JSON) under its canonical root-relative key.
    pub fn stage_text(&mut self, key: &str, text: &str) {
        self.error.clear();
        self.resolver.stage_text(key, text);
    }

    /// Stage a sample resource from raw fetched WAV bytes (decoded here, hound-in-WASM v1).
    pub fn stage_sample_wav(&mut self, key: &str, bytes: &[u8]) -> Result<(), String> {
        self.error.clear();
        match decode_wav_bytes(bytes) {
            Ok(buf) => {
                self.resolver.stage_sample(key, buf);
                Ok(())
            }
            Err(e) => {
                self.error = format!("stage sample {key}: {e}");
                Err(self.error.clone())
            }
        }
    }

    /// Attempt to construct the Engine from the staged document + bundle.
    ///
    /// Fetch-on-miss: resource misses during the load are recorded, not fatal (the core
    /// degrades a missing sample to silence with a warning) — so a load that *succeeded* but
    /// recorded misses is deliberately **not** kept: the caller stages what was asked for and
    /// constructs again, and only a zero-miss load goes live. `log` receives the load
    /// warnings of a successful construct (they are authoring errors the shell must surface).
    ///
    /// Note this drops any live Engine up front — a construct attempt that ends in `Misses`
    /// leaves the shell silent (render returns `false`) until a later attempt goes `Ready`.
    /// Accepted P2 lifecycle: discovery runs on the main-thread instance, so the worklet
    /// normally constructs exactly once per toy, from a complete bundle.
    pub fn construct(&mut self, sample_rate: f32, log: &mut dyn FnMut(&str)) -> ConstructStatus {
        self.engine = None;
        self.misses.clear();
        self.error.clear();
        if !(sample_rate.is_finite() && sample_rate > 0.0) {
            self.error = format!("bad sample rate: {sample_rate}");
            return ConstructStatus::Failed;
        }
        let Some(doc) = self.document.clone() else {
            self.error = "no document staged".to_string();
            return ConstructStatus::Failed;
        };
        // Drop stale misses from any earlier attempt so this round's list is exact.
        let _ = self.resolver.take_misses();

        let result = Engine::from_document(
            &doc,
            &Registry::builtin(),
            &self.resolver,
            AudioConfig::new(sample_rate, BLOCK),
        );
        let misses = self.resolver.take_misses();
        if !misses.is_empty() {
            self.misses = misses;
            return ConstructStatus::Misses;
        }
        match result {
            Ok((engine, warnings)) => {
                for w in &warnings {
                    log(&format!("warning: {w}"));
                }
                if engine.channels() > MAX_CHANNELS {
                    self.error = format!(
                        "instrument wants {} logical output channels; the stereo worklet \
                         carries at most {MAX_CHANNELS} (stereo-sub is out of scope for P2)",
                        engine.channels()
                    );
                    return ConstructStatus::Failed;
                }
                if engine.input_channels() > MAX_INPUT_CHANNELS {
                    self.error = format!(
                        "instrument binds {} logical input channels; the shell carries at \
                         most {MAX_INPUT_CHANNELS}",
                        engine.input_channels()
                    );
                    return ConstructStatus::Failed;
                }
                self.engine = Some(engine);
                ConstructStatus::Ready
            }
            Err(e) => {
                self.error = match e {
                    FromDocumentError::Load(e) => format!("load instrument: {e}"),
                    FromDocumentError::Plan(e) => format!("instantiate plan: {e:?}"),
                };
                ConstructStatus::Failed
            }
        }
    }

    /// The misses recorded by the last construct attempt, in first-miss order.
    pub fn misses(&self) -> &[Miss] {
        &self.misses
    }

    /// Why the last operation failed (empty if it didn't).
    pub fn error(&self) -> &str {
        &self.error
    }

    /// Queue one control message from a flat tagged buffer (the worklet's `postMessage`
    /// payload; see [`crate::codec`]). Returns `Err` with the diagnostic on a malformed
    /// buffer or when no Engine is live — control-channel drift should be loud in the log,
    /// not silent.
    pub fn queue_control(&mut self, bytes: &[u8]) -> Result<(), String> {
        let Some(engine) = self.engine.as_mut() else {
            return Err("queue_control before a successful construct".to_string());
        };
        match decode_control(bytes) {
            Ok((address, args)) => {
                engine.queue_osc(&address, &args);
                Ok(())
            }
            Err(e) => Err(format!("bad control buffer: {e}")),
        }
    }

    /// Render one 128-frame quantum into the planar output buffer.
    ///
    /// `has_input`: the caller staged one quantum of real input into [`WebShell::input_mut`]
    /// (planar). When `false`, the Engine renders through its no-input path — which is what
    /// keeps "no mic stream" distinct from "silent mic" (an input pipe's declared default
    /// materializes until real input has ever been staged; see `Engine::fill_duplex`).
    /// Returns `false` if no Engine is live (the caller keeps outputting silence).
    pub fn render(&mut self, has_input: bool) -> bool {
        let Some(engine) = self.engine.as_mut() else {
            return false;
        };
        let ch = engine.channels();
        let in_ch = engine.input_channels();
        let out = &mut self.scratch_out[..BLOCK * ch];
        if has_input && in_ch > 0 {
            // Planar staged input -> the interleaved logical input master `fill_duplex`
            // expects (one input frame per output frame; the browser already resampled the
            // capture stream to the AudioContext rate — the ADR-0038 §8 seam).
            for f in 0..BLOCK {
                for c in 0..in_ch {
                    self.scratch_in[f * in_ch + c] = self.input[c * BLOCK + f];
                }
            }
            engine.fill_duplex(&self.scratch_in[..BLOCK * in_ch], out);
        } else {
            engine.fill(out);
        }
        // v1 has no outbound door (no osc_out target in the browser yet); drain so the
        // buffer never accumulates across quanta.
        engine.drain_outbound();
        // Interleaved logical frames -> the planar layout the worklet copies per channel.
        for c in 0..ch {
            for f in 0..BLOCK {
                self.out[c * BLOCK + f] = self.scratch_out[f * ch + c];
            }
        }
        true
    }

    /// Logical output channel count of the live Engine (`0` before construct).
    pub fn channels(&self) -> usize {
        self.engine.as_ref().map_or(0, Engine::channels)
    }

    /// Logical input channel count of the live Engine (`0` before construct / no input).
    pub fn input_channels(&self) -> usize {
        self.engine.as_ref().map_or(0, Engine::input_channels)
    }

    /// The rendered quantum, planar `[ch * BLOCK + f]` — the worklet copies
    /// `out[c*BLOCK..(c+1)*BLOCK]` into output channel `c`.
    pub fn out(&self) -> &[f32] {
        &self.out
    }

    /// The input staging buffer, planar `[ch * BLOCK + f]` — the worklet writes
    /// `inputs[0][c]` into `input[c*BLOCK..(c+1)*BLOCK]` before a `has_input` render.
    pub fn input_mut(&mut self) -> &mut [f32] {
        &mut self.input
    }

    /// Tear down for a toy switch: drop the Engine, the staged bundle, and the document.
    /// The instance is reusable — stage and construct again.
    pub fn destroy(&mut self) {
        self.engine = None;
        self.document = None;
        self.resolver.clear();
        self.misses.clear();
        self.error.clear();
        self.out = [0.0; MAX_CHANNELS * BLOCK];
        self.input = [0.0; MAX_INPUT_CHANNELS * BLOCK];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::ResourceKind;

    const VIBRATO: &str = include_str!("../../../instruments/vibrato.json");
    const METRONOME: &str = include_str!("../../../instruments/metronome.json");
    const SAMPLER: &str = include_str!("../../../instruments/sampler.json");
    const SAMPLER_VOICE: &str = include_str!("../../../instruments/voices/sampler-voice.json");
    const BLIP_WAV: &[u8] = include_bytes!("../../../instruments/samples/blip.wav");

    fn no_log() -> impl FnMut(&str) {
        |_: &str| {}
    }

    fn peak(buf: &[f32]) -> f32 {
        buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
    }

    /// Render `n` quanta and return the peak across all of them.
    fn render_peak(shell: &mut WebShell, n: usize) -> f32 {
        let mut p = 0.0f32;
        for _ in 0..n {
            assert!(shell.render(false), "render before ready");
            assert!(
                shell.out().iter().all(|s| s.is_finite()),
                "non-finite sample"
            );
            p = p.max(peak(shell.out()));
        }
        p
    }

    #[test]
    fn construct_and_render_a_self_playing_instrument() {
        let mut shell = WebShell::new();
        shell.set_document(VIBRATO);
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Ready,
            "vibrato: {}",
            shell.error()
        );
        assert_eq!(shell.channels(), 2);
        assert!(
            render_peak(&mut shell, 40) > 0.05,
            "vibrato rendered near-silence"
        );
    }

    #[test]
    fn fetch_on_miss_stages_until_construct_succeeds() {
        // The discovery loop in miniature: construct, read misses, stage, repeat — the JS
        // loader does exactly this against fetch(assetBase + key).
        let mut shell = WebShell::new();
        shell.set_document(SAMPLER);

        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Misses
        );
        assert_eq!(shell.misses().len(), 1);
        assert_eq!(shell.misses()[0].key, "voices/sampler-voice.json");
        assert_eq!(shell.misses()[0].kind, ResourceKind::Text);

        shell.stage_text("voices/sampler-voice.json", SAMPLER_VOICE);
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Misses
        );
        assert_eq!(shell.misses().len(), 1);
        assert_eq!(shell.misses()[0].key, "samples/blip.wav");
        assert_eq!(shell.misses()[0].kind, ResourceKind::Sample);

        shell
            .stage_sample_wav("samples/blip.wav", BLIP_WAV)
            .expect("blip decodes");
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Ready,
            "sampler: {}",
            shell.error()
        );
        // Note-driven: loads and renders finitely without error (the matrix criterion).
        render_peak(&mut shell, 8);
    }

    #[test]
    fn a_control_message_changes_the_output() {
        // /clock/tempo on the metronome: quadrupling the tempo must move the click
        // positions, so the two renders diverge — the canonical control-channel check.
        let render_run = |tempo: Option<f32>| -> Vec<f32> {
            let mut shell = WebShell::new();
            shell.set_document(METRONOME);
            assert_eq!(
                shell.construct(48_000.0, &mut no_log()),
                ConstructStatus::Ready,
                "metronome: {}",
                shell.error()
            );
            if let Some(t) = tempo {
                let buf = crate::codec::encode_control(
                    "/clock/tempo",
                    &[reuben_core::message::Arg::F32(t)],
                );
                shell.queue_control(&buf).expect("queue tempo");
            }
            let mut all = Vec::new();
            for _ in 0..200 {
                shell.render(false);
                all.extend_from_slice(shell.out());
            }
            all
        };
        let base = render_run(None);
        let fast = render_run(Some(480.0));
        assert!(base.iter().all(|s| s.is_finite()));
        assert!(fast.iter().all(|s| s.is_finite()));
        assert!(peak(&base) > 0.01, "metronome silent");
        assert_ne!(base, fast, "/clock/tempo did not change the output");
    }

    #[test]
    fn duplex_input_passes_through_one_block_late() {
        // A minimal input-bound patch (the mic-space shape without its subpatch): staged
        // planar input must come back out, one core block later (Engine's causal alignment).
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
        let mut shell = WebShell::new();
        shell.set_document(MIC);
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Ready,
            "mic: {}",
            shell.error()
        );
        assert_eq!(shell.input_channels(), 1);

        let sig = |f: usize| (f % 89) as f32 / 89.0 - 0.5;
        // Quantum 0: stage input block 0; comes out during quantum 1.
        for f in 0..BLOCK {
            shell.input_mut()[f] = sig(f);
        }
        assert!(shell.render(true));
        assert!(
            shell.out().iter().all(|&s| s == 0.0),
            "first block must be silence (one-block input latency)"
        );
        for f in 0..BLOCK {
            shell.input_mut()[f] = sig(BLOCK + f);
        }
        assert!(shell.render(true));
        for f in 0..BLOCK {
            assert_eq!(
                shell.out()[f].to_bits(),
                sig(f).to_bits(),
                "frame {f}: staged input must land one block later"
            );
        }
    }

    #[test]
    fn destroy_then_reload_switches_toys() {
        let mut shell = WebShell::new();
        shell.set_document(VIBRATO);
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Ready
        );
        assert!(shell.render(false));

        shell.destroy();
        assert!(!shell.render(false), "render must refuse after destroy");
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Failed,
            "construct must refuse with no document"
        );

        shell.set_document(METRONOME);
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Ready,
            "reload after destroy: {}",
            shell.error()
        );
        assert!(render_peak(&mut shell, 100) > 0.01, "metronome silent");
    }

    #[test]
    fn more_than_two_logical_output_channels_is_refused_not_smeared() {
        // The stereo-sub shape: output pipes binding logical channels 0/1/2 derive a
        // 3-channel master, which the stereo worklet node can't carry (issue #224: OUT of
        // the matrix). Construct must refuse with a readable reason, not channel-smear.
        const THREE_OUT: &str = r#"{
          "format_version": 2,
          "instrument": "three_out",
          "interface": {
            "outputs": {
              "l":   { "from": "/osc", "channel": 0 },
              "r":   { "from": "/osc", "channel": 1 },
              "sub": { "from": "/osc", "channel": 2 }
            }
          },
          "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": 110.0 } }
          ]
        }"#;
        let mut shell = WebShell::new();
        shell.set_document(THREE_OUT);
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Failed
        );
        assert!(
            shell.error().contains("logical output channels"),
            "unexpected error: {}",
            shell.error()
        );
    }

    #[test]
    fn has_input_render_on_an_inputless_instrument_is_the_plain_fill_path() {
        // The worklet will happily hand mic input to a toy with no input pipes; render(true)
        // must fall through to the no-input path, not misfeed the Engine.
        let mut shell = WebShell::new();
        shell.set_document(VIBRATO);
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Ready
        );
        assert_eq!(shell.input_channels(), 0);
        shell.input_mut().fill(0.7);
        let mut p = 0.0f32;
        for _ in 0..40 {
            assert!(shell.render(true));
            assert!(shell.out().iter().all(|s| s.is_finite()));
            p = p.max(peak(shell.out()));
        }
        assert!(p > 0.05, "vibrato should still sound through render(true)");
    }

    #[test]
    fn construct_failures_and_bad_input_are_reported_not_panics() {
        let mut shell = WebShell::new();
        // No document.
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Failed
        );
        assert!(!shell.error().is_empty());
        // Bad sample rate.
        shell.set_document(VIBRATO);
        assert_eq!(
            shell.construct(f32::NAN, &mut no_log()),
            ConstructStatus::Failed
        );
        // Malformed document.
        shell.set_document("{ not json");
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Failed
        );
        assert!(shell.error().contains("load instrument"));
        // Control before construct / malformed control buffer.
        assert!(shell.queue_control(&[1, 2, 3]).is_err());
        shell.set_document(VIBRATO);
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Ready
        );
        assert!(shell.queue_control(&[1, 2, 3]).is_err());
        // Bad WAV bytes.
        assert!(shell.stage_sample_wav("x.wav", b"junk").is_err());
    }
}
