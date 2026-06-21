//! Integration: the vibrato example loads, instantiates, and renders a self-playing drone
//! with no input messages — producing finite, non-silent audio.

use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::{load, AudioConfig, Registry};

const VIBRATO_JSON: &str = include_str!("../../../instruments/vibrato.json");

#[test]
fn vibrato_example_renders_a_self_playing_drone() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let graph = load(VIBRATO_JSON, &reg).expect("load vibrato.json");
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    // ~0.5 s of audio, no input messages: the LFO -> osc drone should sound on its own.
    let blocks = (cfg.sample_rate * 0.5) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut peak = 0.0f32;
    for _ in 0..blocks {
        r.render_block(&mut plan, &[], &mut buf);
        for &s in &buf {
            assert!(s.is_finite(), "non-finite output sample: {s}");
            peak = peak.max(s.abs());
        }
    }

    assert!(
        peak > 0.05,
        "vibrato drone produced near-silence (peak {peak})"
    );
}
