//! Integration: the playable echo rig (voicer -> osc -> filter -> env -> delay -> out)
//! loads from JSON and renders without panicking while holding a note.

use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load, AudioConfig, Registry};

const ECHO_JSON: &str = include_str!("../../../instruments/echo.json");

#[test]
fn echo_instrument_loads_and_renders_a_held_note() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let graph = load(ECHO_JSON, &Registry::builtin()).expect("load echo.json");
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    let blocks = (cfg.sample_rate * 1.0) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut peak = 0.0f32;
    for b in 0..blocks {
        let msgs: Vec<Message> = if b == 0 {
            vec![Message::new(
                "/voicer/notes",
                Note::new(Pitch::Absolute(69.0), 1.0),
                0,
            )]
        } else {
            Vec::new()
        };
        r.render_block(&mut plan, &msgs, &mut buf);
        for &s in &buf {
            assert!(s.is_finite(), "non-finite sample in echo render");
            peak = peak.max(s.abs());
        }
    }

    assert!(peak > 0.05, "echo rig produced near-silence (peak {peak})");
}
