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
use reuben_core::format::NormalizedDoc;
use reuben_core::introspect::{describe, describe_patch, validate as validate_doc};
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
    /// The last authoring-introspection result (issue #352, ADR-0052 §2), serialized JSON —
    /// `{ operators }` (describe_operators), a `PatchBoundary` (describe_instrument), or the
    /// contract `Report` (validate). The bridge exposes it via `report_ptr`/`report_len`,
    /// mirroring `error`.
    report: String,
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
            report: String::new(),
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

    /// The last authoring-introspection result as serialized JSON (empty when the last call
    /// produced none — e.g. an errored `describe_*`). The bridge exposes it via
    /// `report_ptr`/`report_len`.
    pub fn report(&self) -> &str {
        &self.report
    }

    /// Serialize `value` into the report buffer and return `true`; on a serialize failure
    /// (infallible in practice for these plain view types) record it as an error and return
    /// `false`. Factors the `describe_*` success path out of its two callers so the
    /// serialize-or-error idiom lives in one place.
    fn store_report(&mut self, serialized: serde_json::Result<String>, label: &str) -> bool {
        match serialized {
            Ok(json) => {
                self.report = json;
                true
            }
            Err(e) => {
                self.error = format!("serialize {label}: {e}");
                false
            }
        }
    }

    /// Describe the operator set (ADR-0052 §2), over [`describe`]: `None` lists every
    /// registered operator, `Some(name)` just that one. On success the report holds
    /// `{ "operators": OperatorInfo[] }` (the `describe_operators` tool contract, ADR-0048 §5)
    /// and this returns `true`; an unknown type sets [`error`](Self::error) and returns `false`.
    pub fn describe_operators(&mut self, which: Option<&str>) -> bool {
        self.error.clear();
        self.report.clear();
        match describe(&Registry::builtin(), which) {
            Ok(ops) => {
                let json = serde_json::to_string(&serde_json::json!({ "operators": ops }));
                self.store_report(json, "operators")
            }
            Err(e) => {
                self.error = e;
                false
            }
        }
    }

    /// Describe an instrument document's boundary (ADR-0052 §2), over [`describe_patch`]. On
    /// success the report holds the `PatchBoundary` JSON (the `describe_instrument` contract)
    /// and this returns `true`; a document that fails to load sets [`error`](Self::error) and
    /// returns `false` (a load failure is `isError`, ADR-0048 §3). Either way the resolver's
    /// misses are drained so introspection never pollutes a later construct's discovery list.
    pub fn describe_instrument(&mut self, json: &str) -> bool {
        self.error.clear();
        self.report.clear();
        let result = describe_patch(json, &Registry::builtin(), &self.resolver);
        // Drain: an introspection miss must not leak into the next construct's miss-list.
        let _ = self.resolver.take_misses();
        match result {
            Ok(boundary) => self.store_report(serde_json::to_string(&boundary), "boundary"),
            Err(e) => {
                self.error = e;
                false
            }
        }
    }

    /// Validate an instrument document (ADR-0052 §2), over [`validate_doc`]. This **always**
    /// produces a report — even a `{ ok: false }` one — and never sets [`error`](Self::error):
    /// a failed validation is a successful call (ADR-0048 §3). The contract [`Report`] JSON
    /// (ADR-0048 §4) lands in the report buffer, the exact type the native lane serializes
    /// (ADR-0052 §5). The resolver's misses are drained so a staged-resource stat during
    /// validation never pollutes a later construct's discovery list.
    ///
    /// [`Report`]: reuben_core::Report
    pub fn validate(&mut self, json: &str) {
        self.error.clear();
        self.report.clear();
        let report = validate_doc(json, &Registry::builtin(), &self.resolver);
        let _ = self.resolver.take_misses();
        // Serializing a Report is infallible in practice; empty-string on the impossible path
        // keeps this method panic-free without inventing a call-level error.
        self.report = serde_json::to_string(&report).unwrap_or_default();
    }

    /// Hash a document's content identity (issue #353, ADR-0052 §3), over
    /// [`reuben_core::content_hash`] — the `content_hash` the in-page `swap` and
    /// `get_current_instrument` contracts carry. Normalizes the document (the same
    /// [`NormalizedDoc`] path the native lane hashes) and hashes its canonical bytes, so the
    /// token is byte-identical to native's for equal content (ADR-0052 §5: one algorithm, two
    /// doors). On success the report holds the opaque hash token and this returns `true`; a
    /// document that fails to normalize sets [`error`](Self::error) and returns `false`. The
    /// resolver's misses are drained so a staged-resource stat here never pollutes a later
    /// construct's discovery list (as `validate`/`describe_instrument` do).
    pub fn content_hash(&mut self, json: &str) -> bool {
        self.error.clear();
        self.report.clear();
        let result = NormalizedDoc::from_json(json, &Registry::builtin(), Some(&self.resolver));
        // Drain: a normalization miss must not leak into the next construct's miss-list.
        let _ = self.resolver.take_misses();
        match result {
            Ok(doc) => {
                self.report = reuben_core::content_hash(&doc);
                true
            }
            Err(e) => {
                self.error = e.to_string();
                false
            }
        }
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
        self.report.clear();
        self.out = [0.0; MAX_CHANNELS * BLOCK];
        self.input = [0.0; MAX_INPUT_CHANNELS * BLOCK];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolver::ResourceKind;

    const VIBRATO: &str = include_str!("../tests/fixtures/vibrato.json");
    const METRONOME: &str = include_str!("../tests/fixtures/metronome.json");
    const SAMPLER: &str = include_str!("../tests/fixtures/sampler.json");
    const SAMPLER_VOICE: &str = include_str!("../tests/fixtures/voices/sampler-voice.json");
    const BLIP_WAV: &[u8] = include_bytes!("../tests/fixtures/samples/blip.wav");

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

    // --- authoring introspection (issue #352, ADR-0052 §2): describe_operators /
    // describe_instrument / validate, over reuben_core::introspect — the exact OS-free
    // contract types the native lane serializes (§5: one schema, two doors). ---------------

    #[test]
    fn describe_operators_all_lists_the_registry() {
        let mut shell = WebShell::new();
        assert!(
            shell.describe_operators(None),
            "describe all: {}",
            shell.error()
        );
        let v: serde_json::Value = serde_json::from_str(shell.report()).expect("report is JSON");
        let ops = v["operators"].as_array().expect("operators is an array");
        let names: Vec<&str> = ops
            .iter()
            .map(|o| o["type_name"].as_str().unwrap())
            .collect();
        for expected in ["oscillator", "filter", "voicer"] {
            assert!(names.contains(&expected), "missing {expected} in {names:?}");
        }
        assert!(shell.error().is_empty(), "ok call leaves no error");
    }

    #[test]
    fn describe_operators_one_returns_a_single_op_with_its_ports() {
        let mut shell = WebShell::new();
        assert!(shell.describe_operators(Some("oscillator")));
        let v: serde_json::Value = serde_json::from_str(shell.report()).unwrap();
        let ops = v["operators"].as_array().unwrap();
        assert_eq!(ops.len(), 1, "one named op");
        assert_eq!(ops[0]["type_name"], serde_json::json!("oscillator"));
        assert!(
            ops[0]["inputs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["name"] == serde_json::json!("freq")),
            "oscillator surfaces a freq input: {}",
            shell.report()
        );
    }

    #[test]
    fn describe_operators_unknown_is_an_error_with_no_report() {
        let mut shell = WebShell::new();
        assert!(!shell.describe_operators(Some("nope")));
        assert!(
            shell.error().contains("nope"),
            "error names the missing type: {}",
            shell.error()
        );
        assert!(shell.report().is_empty(), "no report on an errored call");
    }

    #[test]
    fn describe_instrument_surfaces_the_boundary() {
        let mut shell = WebShell::new();
        assert!(
            shell.describe_instrument(VIBRATO),
            "vibrato: {}",
            shell.error()
        );
        let v: serde_json::Value = serde_json::from_str(shell.report()).unwrap();
        assert_eq!(
            v["instrument"],
            serde_json::json!("vibrato"),
            "boundary names the document's instrument"
        );
        assert!(shell.error().is_empty());
    }

    #[test]
    fn describe_instrument_bad_json_is_an_error() {
        let mut shell = WebShell::new();
        assert!(!shell.describe_instrument("{ not json"));
        assert!(
            !shell.error().is_empty(),
            "a doc that fails to load is isError"
        );
        assert!(shell.report().is_empty(), "no report on an errored call");
    }

    #[test]
    fn validate_a_good_document_reports_ok() {
        let mut shell = WebShell::new();
        shell.validate(VIBRATO);
        let report: reuben_core::Report =
            serde_json::from_str(shell.report()).expect("report deserializes as the native type");
        assert!(report.ok, "vibrato should validate: {:?}", report.errors);
        assert!(report.errors.is_empty(), "no errors: {:?}", report.errors);
        assert!(
            shell.error().is_empty(),
            "a validation is never a call-level error"
        );
    }

    #[test]
    fn validate_a_broken_document_is_a_successful_call_reporting_ok_false() {
        // ADR-0048 §3: a failed validation is a SUCCESSFUL call — it produces a report
        // (even `{ok:false}`), never a call-level error.
        const BROKEN: &str = r#"{ "instrument": "typo",
          "nodes": [ { "type": "oscilllator", "address": "/osc" } ], "outputs": [] }"#;
        let mut shell = WebShell::new();
        shell.validate(BROKEN);
        // Drift guard (ADR-0052 §5, "one schema, two doors" made executable): the
        // browser-serialized bytes deserialize cleanly into the native contract type.
        let report: reuben_core::Report =
            serde_json::from_str(shell.report()).expect("one schema, two doors");
        assert!(!report.ok, "unknown operator fails validation");
        assert!(!report.errors.is_empty(), "at least one diag");
        assert_eq!(
            report.errors[0].node.as_deref(),
            Some("/osc"),
            "the diag localizes the offending node: {:?}",
            report.errors[0]
        );
        assert!(
            shell.error().is_empty(),
            "validate never sets error, even on ok:false"
        );
    }

    #[test]
    fn introspection_does_not_pollute_a_later_construct_miss_list() {
        // describe_instrument/validate drain the resolver's misses so a subsequent construct's
        // discovery miss-list stays exact — no leaked introspection miss.
        const GHOST: &str = r#"{ "format_version": 3, "instrument": "ghost",
          "resources": { "v": "voices/nope.json" },
          "nodes": [
            { "type": "voicer", "address": "/voicer", "voice": "v", "config": { "voices": 1 } },
            { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/voicer.audio" } } }
          ],
          "outputs": [ { "node": "/out", "port": "audio" } ] }"#;
        let mut shell = WebShell::new();
        shell.validate(GHOST); // records then drains a miss for voices/nope.json
        let _ = shell.describe_instrument(GHOST); // ditto

        // The sampler's own discovery must report exactly its own first miss.
        shell.set_document(SAMPLER);
        assert_eq!(
            shell.construct(48_000.0, &mut no_log()),
            ConstructStatus::Misses
        );
        assert_eq!(shell.misses().len(), 1, "exactly the sampler's own miss");
        assert_eq!(shell.misses()[0].key, "voices/sampler-voice.json");
    }

    #[test]
    fn validate_stats_staged_resources_and_reports_clean() {
        // Spec #352: "validate includes the staged-resource stat." The positive path — once the
        // referenced resources are staged, validate stats them as resolved and reports
        // warning-clean (the unstaged/miss path is
        // introspection_does_not_pollute_a_later_construct_miss_list above).
        let mut shell = WebShell::new();
        shell.stage_text("voices/sampler-voice.json", SAMPLER_VOICE);
        shell
            .stage_sample_wav("samples/blip.wav", BLIP_WAV)
            .expect("blip decodes");
        shell.validate(SAMPLER);
        let report: reuben_core::Report =
            serde_json::from_str(shell.report()).expect("one schema, two doors");
        assert!(report.ok, "sampler validates: {:?}", report.errors);
        assert!(
            report.warnings.is_empty(),
            "staged resources stat clean: {:?}",
            report.warnings
        );
    }

    // --- content_hash (issue #353, ADR-0052 §3): the fourth authoring export the in-page
    // `swap`/`get_current_instrument` contracts carry — over reuben_core::content_hash, so the
    // token is byte-identical to what the native lane mints (ADR-0052 §5). ------------------

    const HASHME: &str = r#"{ "format_version": 3, "instrument": "hashme",
      "interface": { "outputs": { "out": { "from": "/out.audio" } } },
      "nodes": [
        { "type": "oscillator", "address": "/osc", "inputs": { "freq": 220.0 } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/osc" } } }
      ] }"#;

    #[test]
    fn content_hash_is_stable_and_equal_for_an_equal_doc() {
        let mut shell = WebShell::new();
        assert!(shell.content_hash(HASHME), "hash HASHME: {}", shell.error());
        let first = shell.report().to_string();
        assert!(!first.is_empty(), "hash is a non-empty token");
        assert!(shell.error().is_empty(), "an ok hash leaves no error");

        // Same doc again -> same token (stable).
        assert!(shell.content_hash(HASHME));
        assert_eq!(shell.report(), first, "equal doc hashes equal");

        // Reformatted-but-equal doc (extra whitespace) -> same token: the hash is over the
        // canonical bytes, not the source text (ADR-0046 §9).
        let reformatted = format!("  {}\n\n", HASHME.replace('\n', "  "));
        assert!(shell.content_hash(&reformatted));
        assert_eq!(
            shell.report(),
            first,
            "canonicalization: whitespace does not change the hash"
        );
    }

    #[test]
    fn content_hash_differs_on_a_changed_node() {
        let changed = HASHME.replace("\"freq\": 220.0", "\"freq\": 440.0");
        let mut shell = WebShell::new();
        assert!(shell.content_hash(HASHME));
        let before = shell.report().to_string();
        assert!(shell.content_hash(&changed));
        assert_ne!(shell.report(), before, "a changed node changes the hash");
    }

    #[test]
    fn content_hash_of_an_unparseable_doc_is_an_error_with_no_report() {
        let mut shell = WebShell::new();
        assert!(!shell.content_hash("{ not json"));
        assert!(
            !shell.error().is_empty(),
            "a doc that fails to parse sets an error"
        );
        assert!(shell.report().is_empty(), "no report on an errored hash");
    }

    #[test]
    fn content_hash_byte_equals_the_native_contract() {
        // The #353 drift guard (ADR-0052 §5, one algorithm two doors): the browser-minted
        // token equals hashing the native NormalizedDoc directly.
        let native = reuben_core::content_hash(
            &NormalizedDoc::from_json(HASHME, &Registry::builtin(), None).unwrap(),
        );
        let mut shell = WebShell::new();
        assert!(shell.content_hash(HASHME));
        assert_eq!(shell.report(), native, "content_hash bytes match native");
    }

    #[test]
    fn web_serialization_byte_equals_the_native_contract_for_all_three() {
        // The #352 drift guard, literal (ADR-0052 §5, one schema two doors): the
        // browser-serialized bytes equal serializing the native contract types directly.
        // Self-contained inputs, so the resolver choice cannot diverge the result.
        let reg = Registry::builtin();
        let resolver = WebResolver::new();

        let mut shell = WebShell::new();
        assert!(shell.describe_operators(None));
        let native = serde_json::to_string(
            &serde_json::json!({ "operators": describe(&reg, None).unwrap() }),
        )
        .unwrap();
        assert_eq!(
            shell.report(),
            native,
            "describe_operators bytes match native"
        );

        assert!(shell.describe_instrument(VIBRATO));
        let native =
            serde_json::to_string(&describe_patch(VIBRATO, &reg, &resolver).unwrap()).unwrap();
        assert_eq!(
            shell.report(),
            native,
            "describe_instrument bytes match native"
        );

        shell.validate(VIBRATO);
        let native = serde_json::to_string(&validate_doc(VIBRATO, &reg, &resolver)).unwrap();
        assert_eq!(shell.report(), native, "validate bytes match native");
    }
}
