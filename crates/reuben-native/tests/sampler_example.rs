//! Integration: the one-shot sampler rig (voicer -> sample -> out) loads from JSON with a
//! filesystem WAV resolver, binds the decoded `blip.wav`, and makes sound on a note (ADR-0016).

use std::path::PathBuf;

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::{load_instrument, AudioConfig, Registry};
use reuben_native::resources::FsResolver;

/// Absolute path to the workspace `instruments/` directory, independent of test CWD.
fn instruments_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../instruments")
}

#[test]
fn sampler_loads_resolves_wav_and_plays_a_note() {
    let dir = instruments_dir();
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
                "/voicer/note",
                [Arg::Float(57.0), Arg::Float(1.0)],
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
fn missing_sample_warns_but_still_loads() {
    // A resources table pointing at a nonexistent file: load succeeds with a warning, and
    // the node plays silence rather than crashing (ADR-0016 degrade-to-silence).
    let json = r#"{
      "instrument": "broken",
      "resources": { "ghost": "samples/does_not_exist.wav" },
      "nodes": [
        { "type": "voicer", "address": "/voicer", "params": { "voices": 1 } },
        { "type": "sample", "address": "/s", "sample": "ghost" },
        { "type": "output", "address": "/out" }
      ],
      "connections": [
        { "from": {"node":"/voicer","port":"freq"}, "to": {"node":"/s","port":"freq"} },
        { "from": {"node":"/voicer","port":"gate"}, "to": {"node":"/s","port":"gate"} },
        { "from": {"node":"/s","port":"audio"}, "to": {"node":"/out","port":"audio"} }
      ],
      "outputs": [ {"node":"/out","port":"audio"} ]
    }"#;
    let resolver = FsResolver::new(instruments_dir());
    let loaded = load_instrument(json, &Registry::builtin(), &resolver).expect("loads anyway");
    assert_eq!(loaded.warnings.len(), 1, "expected one resolve warning");

    let cfg = AudioConfig::new(48_000.0, 256);
    let mut plan = Plan::instantiate(loaded.graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; cfg.block_size];
    r.render_block(
        &mut plan,
        &[Message::new(
            "/voicer/note",
            [Arg::Float(60.0), Arg::Float(1.0)],
            0,
        )],
        &mut buf,
    );
    assert!(
        buf.iter().all(|&s| s == 0.0),
        "missing sample should be silent"
    );
}
