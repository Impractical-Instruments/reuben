//! Integration: the one-shot sampler rig (voicer -> sample -> out) loads from JSON with a
//! filesystem WAV resolver, binds the decoded `blip.wav`, and makes sound on a note.
//!
//! Driven through the window's render pair, the way `audio.rs` drives it: install, then
//! `queue_osc` + `fill` on the slot.

use std::path::PathBuf;

use reuben_api::render::{install_initial, Arg, AudioConfig, RenderSlot};
use reuben_api::resources::{ResolveError, Resources, SampleBuffer};
use reuben_api::FsResolver;

/// Absolute path to this crate's frozen test fixtures, independent of test CWD.
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// The config every case here renders at.
fn cfg() -> AudioConfig {
    AudioConfig::new(48_000.0, 256)
}

#[test]
fn sampler_loads_resolves_wav_and_plays_a_note() {
    let dir = fixtures_dir();
    let json = std::fs::read_to_string(dir.join("sampler.json")).expect("read sampler.json");

    let cfg = cfg();
    let (_coordinator, side, warnings) =
        install_initial(&json, FsResolver::new(&dir), cfg).expect("install sampler.json");
    // The blip resolves cleanly — no warnings on the worked example.
    assert!(
        warnings.is_empty(),
        "unexpected load warnings: {warnings:?}"
    );

    let mut slot = RenderSlot::new(side);
    // Fire a note at the sample's root pitch (MIDI 57) and render ~0.25 s. The flat primitive form
    // an OSC datagram arrives in; the slot types it against the destination port.
    slot.queue_osc("/voicer/notes", &[Arg::F32(57.0), Arg::F32(1.0)]);

    let blocks = (cfg.sample_rate * 0.25) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size * slot.channels()];
    let mut peak = 0.0f32;
    for _ in 0..blocks {
        slot.fill(&mut buf);
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

    let cfg = cfg();
    let (_coordinator, side, warnings) =
        install_initial(&json, FsResolver::new(&dir), cfg).expect("install sampler-arp.json");
    assert!(
        warnings.is_empty(),
        "unexpected load warnings: {warnings:?}"
    );

    // ~1 s at 132 BPM is ~2.2 beats — several arpeggio steps fire with no input.
    let mut slot = RenderSlot::new(side);
    let blocks = cfg.sample_rate as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size * slot.channels()];
    let mut peak = 0.0f32;
    for _ in 0..blocks {
        slot.fill(&mut buf);
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
/// degrade-to-silence path (a missing sample *inside* a hosted voice) without
/// committing a deliberately-broken fixture. Sample bytes still resolve through the real FS.
struct GhostVoiceStore(FsResolver);

impl Resources for GhostVoiceStore {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.0.read_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
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
            self.0.read_text(source)
        }
    }
}

#[test]
fn missing_sample_warns_but_still_loads() {
    // A hosted voice whose nested `sample` resource points at a nonexistent file: load succeeds
    // with a warning, and the voice plays silence rather than crashing (degrade-to-silence,
    // resolved recursively through the voice sub-patch).
    let json = r#"{
      "instrument": "broken",
      "resources": { "ghost-voice": "ghost-voice" },
      "nodes": [
        { "type": "voicer", "address": "/voicer", "voice": "ghost-voice", "config": { "voices": 1 } },
        { "type": "output", "address": "/out", "inputs": { "audio": {"from":"/voicer.audio"} } }
      ],
      "outputs": [ {"node":"/out","port":"audio"} ]
    }"#;
    let store = GhostVoiceStore(FsResolver::new(fixtures_dir()));

    let cfg = cfg();
    let (_coordinator, side, warnings) = install_initial(json, store, cfg).expect("loads anyway");
    assert_eq!(warnings.len(), 1, "expected one resolve warning");

    let mut slot = RenderSlot::new(side);
    slot.queue_osc("/voicer/notes", &[Arg::F32(60.0), Arg::F32(1.0)]);
    let mut buf = vec![0.0f32; cfg.block_size * slot.channels()];
    slot.fill(&mut buf);
    assert!(
        buf.iter().all(|&s| s == 0.0),
        "missing sample should be silent"
    );
}
