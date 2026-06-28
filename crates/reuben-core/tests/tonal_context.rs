//! Integration: the tonal-context bus end-to-end (ADR-0013, ADR-0015) — a harmony node
//! publishes the latched key/scale, the engine routes and slices it onto downstream readers,
//! and a Voicer resolves degree notes to Hz through it. Exercises the engine plumbing the
//! operator unit tests can't: the context arena, the third route lane, and sample-accurate
//! re-slicing on a context change.

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::vocab::harmony::Harmony;
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load_instrument, AudioConfig, Registry};

const SCALE_DEMO: &str = include_str!("../../../instruments/scale-demo.json");
const AUTOTUNE: &str = include_str!("../../../instruments/autotune.json");

/// Resolves each rig's `voice` instrument-resource (ADR-0032) from the repo `instruments/` dir.
struct InstrumentsDir;
impl ResourceResolver for InstrumentsDir {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/../../instruments/{source}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }
}

const CFG: AudioConfig = AudioConfig {
    sample_rate: 48_000.0,
    block_size: 256,
    channels: AudioConfig::MIN_CHANNELS,
};

fn hz(midi: f32) -> f32 {
    Harmony::default().hz(Pitch::from_midi(midi))
}

/// Test-only **freq-probe voice** (ADR-0032 session 11): a single `mul_f32_signal` whose `a`
/// operand is the voice's `freq` interface input (f32_buffer-with-meta, message-settable, ZOH-
/// materialized) and whose `b` defaults to 1.0 — so the voice's audio is `freq * 1 == freq`. Hosting
/// it under a Voicer makes `voicer.audio` equal the resolved pitch, recreating the removed
/// `voicer.freq` output tap so the pitch-resolution assertions stay byte-identical.
const FREQ_PROBE_VOICE: &str = r#"{
  "instrument": "freq-probe-voice",
  "interface": { "inputs": { "freq": "/mul.a" }, "outputs": { "audio": "/mul.out" } },
  "nodes": [ { "type": "mul_f32_signal", "address": "/mul" } ],
  "outputs": [ { "node": "/mul", "port": "out" } ]
}"#;

/// Serves the test-only probe voice (and nothing else) as an instrument-resource.
struct ProbeResolver;
impl ResourceResolver for ProbeResolver {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        match source {
            "freq-probe-voice" => Ok(FREQ_PROBE_VOICE.to_string()),
            other => Err(ResolveError::NotFound(other.to_string())),
        }
    }
}

/// Load a host instrument JSON that references the `freq-probe-voice`, and instantiate it.
fn load_probe(host: &str) -> Plan {
    let graph = load_instrument(host, &Registry::builtin(), &ProbeResolver)
        .expect("load probe host")
        .graph;
    Plan::instantiate(graph, CFG).expect("instantiate")
}

/// A minimal `harmony -> voicer(mono, freq-probe-voice)` host, tapping `voicer.audio` (== the
/// resolved pitch) so a test can read it directly.
const CONTEXT_VOICER: &str = r#"{
  "instrument": "context-voicer",
  "nodes": [
    { "type": "harmony", "address": "/harmony" },
    { "type": "voicer", "address": "/voicer", "voice": "v", "config": { "voices": 1 },
      "inputs": { "harmony": { "from": "/harmony" } } },
    { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/voicer.audio" } } }
  ],
  "outputs": [ { "node": "/out", "port": "audio" } ],
  "resources": { "v": "freq-probe-voice" }
}"#;

#[test]
fn degree_note_resolves_then_respells_across_blocks() {
    let mut plan = load_probe(CONTEXT_VOICER);
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    // Block 1: play degree 2. In C major that is E (MIDI 64).
    let note = Message::new("/voicer/notes", Note::new(Pitch::Degree(2), 1.0), 0);
    r.render_block(&mut plan, &[note], &mut buf);
    approx::assert_relative_eq!(buf[CFG.block_size - 1], hz(64.0), epsilon = 1e-2);

    // Block 2: move the root to D (62) with no new note. The held degree 2 re-spells to F♯
    // (66) — proof the latched context drives a live re-spell through the engine, not just
    // the operator.
    let key = Message::new("/harmony/root", Arg::F32(62.0), 0);
    r.render_block(&mut plan, &[key], &mut buf);
    approx::assert_relative_eq!(buf[CFG.block_size - 1], hz(66.0), epsilon = 1e-2);
}

#[test]
fn context_change_mid_block_is_sample_accurate() {
    let mut plan = load_probe(CONTEXT_VOICER);
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    // Hold degree 2 (E in C major).
    r.render_block(
        &mut plan,
        &[Message::new(
            "/voicer/notes",
            Note::new(Pitch::Degree(2), 1.0),
            0,
        )],
        &mut buf,
    );

    // In the next block, change the root to D at frame 128. The change slices the block: the
    // first half still resolves in C major (E), the second half in D major (F♯) — one
    // sample-accurate timeline, no block-quantization internally.
    let key = Message::new("/harmony/root", Arg::F32(62.0), 128);
    r.render_block(&mut plan, &[key], &mut buf);
    approx::assert_relative_eq!(buf[100], hz(64.0), epsilon = 1e-2); // before 128 → E
    approx::assert_relative_eq!(buf[200], hz(66.0), epsilon = 1e-2); // after 128 → F♯
}

/// `harmony -> snap -> voicer(mono, freq-probe-voice)`, tapping `voicer.audio`.
const SNAP_VOICER: &str = r#"{
  "instrument": "snap-voicer",
  "nodes": [
    { "type": "harmony", "address": "/harmony" },
    { "type": "snap", "address": "/snap", "inputs": { "harmony": { "from": "/harmony" } } },
    { "type": "voicer", "address": "/voicer", "voice": "v", "config": { "voices": 1 },
      "inputs": { "notes": { "from": "/snap" }, "harmony": { "from": "/harmony" } } },
    { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/voicer.audio" } } }
  ],
  "outputs": [ { "node": "/out", "port": "audio" } ],
  "resources": { "v": "freq-probe-voice" }
}"#;

#[test]
fn snap_quantizes_an_off_key_gesture() {
    // harmony -> snap -> voicer. Play D♯ (63), an off-scale pitch; the snap pulls it to the
    // nearest C-major tone (D, 62, on the down tie-break) before the Voicer resolves it.
    let mut plan = load_probe(SNAP_VOICER);
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];
    let note = Message::new("/snap/notes", Note::new(Pitch::Absolute(63.0), 1.0), 0);
    r.render_block(&mut plan, &[note], &mut buf);
    approx::assert_relative_eq!(buf[CFG.block_size - 1], hz(62.0), epsilon = 1e-2);
    // D
}

#[test]
fn demo_instruments_load_and_play() {
    let reg = Registry::builtin();
    for json in [SCALE_DEMO, AUTOTUNE] {
        let graph = load_instrument(json, &reg, &InstrumentsDir)
            .expect("load demo instrument")
            .graph;
        let mut plan = Plan::instantiate(graph, CFG).expect("instantiate");
        let mut r = Renderer::new(&plan);
        let mut buf = vec![0.0f32; CFG.block_size];
        // scale-demo self-plays; autotune needs input — drive both so each makes sound.
        let note = Message::new("/snap/notes", Note::new(Pitch::Absolute(67.3), 1.0), 0);
        let mut peak = 0.0f32;
        for _ in 0..400 {
            r.render_block(&mut plan, std::slice::from_ref(&note), &mut buf);
            peak = peak.max(buf.iter().fold(0.0, |m, &s| m.max(s.abs())));
        }
        assert!(peak > 0.01, "demo instrument produced no sound");
    }
}
