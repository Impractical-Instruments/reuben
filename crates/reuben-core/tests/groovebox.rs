//! Integration: the groovebox beatmaker (V1.3 Toy 1, ADR-0022/0032) loads through the full
//! resource pipeline — three lane Voicers each host a drum-synth voice patch — and self-plays a
//! non-silent beat with no external input.
//!
//! (Supersedes the old `groovebox_snare_gate.rs` probes, which tapped the now-removed `voicer.gate`
//! output and internal drum-synth nodes that moved inside the voice patches, ADR-0032.)

use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::{load_instrument, AudioConfig, Registry};

const GROOVEBOX: &str = include_str!("../../../instruments/groovebox.json");

/// Resolves the groovebox's drum-voice instrument-resources from the repo `instruments/` dir.
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

#[test]
fn groovebox_self_plays_a_non_silent_beat() {
    // The clock-driven rig needs no external notes: the three step sequencers fire their lane
    // Voicers, each hosting a drum-synth voice. Render ~2 s (default 120 BPM, several bars) and
    // listen for sound.
    let graph = load_instrument(GROOVEBOX, &Registry::builtin(), &InstrumentsDir)
        .expect("load groovebox")
        .graph;
    let cfg = AudioConfig::new(48_000.0, 256);
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    let blocks = (cfg.sample_rate * 2.0) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut peak = 0.0f32;
    let no_msgs: Vec<Message> = Vec::new();
    for _ in 0..blocks {
        r.render_block(&mut plan, &no_msgs, &mut buf);
        for &s in &buf {
            assert!(s.is_finite(), "non-finite sample in groovebox render");
            peak = peak.max(s.abs());
        }
    }
    assert!(peak > 0.05, "groovebox produced near-silence (peak {peak})");
}
