//! Integration: the Voicer's idle-voice skipping (ADR-0032 §5) with a voice patch that declares
//! an `active` interface output (`default-voice`), so `can_skip` is live — a render path no other
//! test reaches (the freq-probe voices in tonal_context.rs have no `active`, so they always take
//! the render-everything fallback). Two redundant-looking guards keep a within-one-block tap
//! audible — the per-block `touched` flag and `assign()` seeding `active: true` — and each looks
//! individually deletable, so the pin is behavioral (sound, not flags): the attack blip must
//! render even though the voice ends the block gate-off, and the release tail of a skip-eligible
//! voice must keep sounding via the `active` feedback.

use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load_instrument, AudioConfig, Registry};

const DEFAULT_VOICE: &str = include_str!("../../../instruments/voices/default-voice.json");

/// A minimal `voicer(2 × default-voice) -> out` host. `default-voice` declares both `audio` and
/// `active` interface outputs, so the Voicer resolves an `active_cap` and enables idle-voice
/// skipping — the precondition under test.
const TAP_HOST: &str = r#"{
  "instrument": "tap-host",
  "resources": { "v": "voices/default-voice.json" },
  "nodes": [
    { "type": "voicer", "address": "/voicer", "voice": "v", "config": { "voices": 2 } },
    { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/voicer.audio" } } }
  ],
  "outputs": [ { "node": "/out", "port": "audio" } ]
}"#;

/// Resolves the host's `voice` instrument-resource (ADR-0032) from the repo `instruments/` dir.
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
    block_size: 128,
    channels: AudioConfig::MIN_CHANNELS,
    input_channels: 0,
};

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0, |m, s| m.max(s.abs()))
}

#[test]
fn same_block_tap_still_sounds() {
    // Precondition: `default-voice` really exposes `active` as a captured Value output — the
    // exact seam (`interface_value_slot`, the check voice_liveness.rs pins standalone) the
    // Voicer keys `can_skip` on. Assert it here so a renamed/removed `active` fails loudly
    // instead of silently degrading this test to the render-everything fallback.
    let voice_graph = load_instrument(DEFAULT_VOICE, &Registry::builtin(), &InstrumentsDir)
        .expect("load default-voice")
        .graph;
    let voice_plan = Plan::instantiate(voice_graph, CFG).expect("instantiate voice");
    assert!(
        voice_plan.interface_value_slot("active").is_some(),
        "default-voice must declare an `active` Value output (the can_skip precondition)"
    );

    let graph = load_instrument(TAP_HOST, &Registry::builtin(), &InstrumentsDir)
        .expect("load tap host")
        .graph;
    let mut plan = Plan::instantiate(graph, CFG).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    // One block containing a whole tap: note-on at frame 0, its note-off at frame 64. The voice
    // ends the block gate-off, yet its attack blip must render — the guard pair (`touched`, the
    // assign-time `active` seed) is what keeps the skip test from seeing it as idle.
    let msgs = [
        Message::new("/voicer/notes", Note::new(Pitch::Degree(0), 1.0), 0),
        Message::new("/voicer/notes", Note::new(Pitch::Degree(0), 0.0), 64),
    ];
    r.render_block(&mut plan, &msgs, &mut buf);
    assert!(
        peak(&buf) > 0.0,
        "a same-block tap must still render its attack blip"
    );

    // Next block, no messages: the voice is gate-off and untouched, but its release tail
    // (default release 0.2 s >> one block) is still live — idle-skip must key on the fed-back
    // `active`, not on the gate alone, so the tail keeps sounding instead of being cut.
    r.render_block(&mut plan, &[], &mut buf);
    assert!(
        peak(&buf) > 0.0,
        "the release tail of a skip-eligible voice keeps rendering"
    );
}
