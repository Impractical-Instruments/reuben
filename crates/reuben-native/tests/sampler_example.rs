//! Integration: the one-shot sampler rig (voicer -> sample -> out) loads from JSON with a
//! filesystem WAV resolver, binds the decoded `blip.wav`, and makes sound on a note (ADR-0016).

use std::path::PathBuf;

use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load_instrument, AudioConfig, Registry};
use reuben_native::resources::FsResolver;

/// Absolute path to this crate's frozen test fixtures, independent of test CWD.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn sampler_loads_resolves_wav_and_plays_a_note() {
    let dir = fixtures_dir();
    let json = std::fs::read_to_string(dir.join("sampler.json")).expect("read sampler.json");
    let resolver = FsResolver::new(&dir);

    let loaded =
        load_instrument(&json, &Registry::builtin(), &resolver).expect("load sampler.json");
    // The blip resolves cleanly — no warnings on the worked example.
    assert!(
        loaded.warnings.is_empty(),
        "unexpected load warnings: {:?}",
        loaded.warnings
    );

    let cfg = AudioConfig::new(48_000.0, 256);
    let mut plan = Plan::instantiate(loaded.graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    // Fire a note at the sample's root pitch (MIDI 57) and render ~0.25 s.
    let blocks = (cfg.sample_rate * 0.25) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut peak = 0.0f32;
    for b in 0..blocks {
        let msgs: Vec<Message> = if b == 0 {
            vec![Message::new(
                "/voicer/notes",
                Note::new(Pitch::Absolute(57.0), 1.0),
                0,
            )]
        } else {
            Vec::new()
        };
        r.render_block(&mut plan, &msgs, &mut buf);
        for &s in &buf {
            assert!(s.is_finite(), "non-finite sample in sampler render");
            peak = peak.max(s.abs());
        }
    }

    assert!(peak > 0.05, "sampler produced near-silence (peak {peak})");
}

#[test]
fn sampler_arp_self_plays_a_sequenced_arpeggio() {
    // The clock-driven rig needs no external notes: the sequencer emits a major arpeggio
    // into the Voicer, whose gate edges fire the sample. Just render and listen for sound.
    let dir = fixtures_dir();
    let json =
        std::fs::read_to_string(dir.join("sampler-arp.json")).expect("read sampler-arp.json");
    let resolver = FsResolver::new(&dir);

    let loaded =
        load_instrument(&json, &Registry::builtin(), &resolver).expect("load sampler-arp.json");
    assert!(
        loaded.warnings.is_empty(),
        "unexpected load warnings: {:?}",
        loaded.warnings
    );

    let cfg = AudioConfig::new(48_000.0, 256);
    let mut plan = Plan::instantiate(loaded.graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    // ~1 s at 132 BPM is ~2.2 beats — several arpeggio steps fire with no input.
    let blocks = cfg.sample_rate as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut peak = 0.0f32;
    for _ in 0..blocks {
        r.render_block(&mut plan, &[], &mut buf);
        for &s in &buf {
            assert!(s.is_finite(), "non-finite sample in sampler-arp render");
            peak = peak.max(s.abs());
        }
    }

    assert!(
        peak > 0.05,
        "sampler-arp produced near-silence (peak {peak})"
    );
}

/// Wraps an [`FsResolver`] but serves one inline voice patch — a `sampler-voice` whose nested
/// `sample` resource points at a nonexistent file — so the test can exercise the recursive
/// degrade-to-silence path (a missing sample *inside* a hosted voice, ADR-0016/0032) without
/// committing a deliberately-broken fixture. Sample bytes still resolve through the real FS.
struct GhostVoiceResolver(FsResolver);
impl reuben_core::resources::ResourceResolver for GhostVoiceResolver {
    fn resolve(
        &self,
        source: &str,
    ) -> Result<reuben_core::resources::SampleBuffer, reuben_core::resources::ResolveError> {
        self.0.resolve(source)
    }
    fn resolve_text(&self, source: &str) -> Result<String, reuben_core::resources::ResolveError> {
        if source == "ghost-voice" {
            Ok(r#"{
              "instrument": "ghost-voice",
              "interface": { "inputs": { "freq": "/s.freq", "gate": "/s.gate" },
                             "outputs": { "audio": "/out.audio" } },
              "resources": { "ghost": "samples/does_not_exist.wav" },
              "nodes": [
                { "type": "sample", "address": "/s", "sample": "ghost", "inputs": { "root": 57.0 } },
                { "type": "output", "address": "/out", "inputs": { "audio": {"from":"/s"} } }
              ],
              "outputs": [ {"node":"/out","port":"audio"} ]
            }"#
            .to_string())
        } else {
            self.0.resolve_text(source)
        }
    }
}

#[test]
fn missing_sample_warns_but_still_loads() {
    // A hosted voice whose nested `sample` resource points at a nonexistent file: load succeeds
    // with a warning, and the voice plays silence rather than crashing (ADR-0016 degrade-to-silence,
    // resolved recursively through the voice sub-patch, ADR-0032).
    let json = r#"{
      "instrument": "broken",
      "resources": { "ghost-voice": "ghost-voice" },
      "nodes": [
        { "type": "voicer", "address": "/voicer", "voice": "ghost-voice", "config": { "voices": 1 } },
        { "type": "output", "address": "/out", "inputs": { "audio": {"from":"/voicer.audio"} } }
      ],
      "outputs": [ {"node":"/out","port":"audio"} ]
    }"#;
    let resolver = GhostVoiceResolver(FsResolver::new(fixtures_dir()));
    let loaded = load_instrument(json, &Registry::builtin(), &resolver).expect("loads anyway");
    assert_eq!(loaded.warnings.len(), 1, "expected one resolve warning");

    let cfg = AudioConfig::new(48_000.0, 256);
    let mut plan = Plan::instantiate(loaded.graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; cfg.block_size];
    r.render_block(
        &mut plan,
        &[Message::new(
            "/voicer/notes",
            Note::new(Pitch::Absolute(60.0), 1.0),
            0,
        )],
        &mut buf,
    );
    assert!(
        buf.iter().all(|&s| s == 0.0),
        "missing sample should be silent"
    );
}
