//! Integration smoke test: the reverb example rig loads, instantiates, and renders a held
//! note across several blocks without panicking.

use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load, AudioConfig, Registry};

const REVERB_JSON: &str = include_str!("../../../instruments/reverb.json");

#[test]
#[ignore = "ADR-0032 follow-up: depends on a voicer instrument / voicer.freq tap; re-author to the hosted-voice model, then restore"]
fn reverb_example_loads_and_renders_a_held_note() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let graph = load(REVERB_JSON, &Registry::builtin()).expect("load reverb.json");
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    let mut buf = vec![0.0f32; cfg.block_size];
    let mut peak = 0.0f32;
    for b in 0..32 {
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
            assert!(s.is_finite(), "reverb rig produced a non-finite sample");
            peak = peak.max(s.abs());
        }
    }

    assert!(
        peak > 0.01,
        "reverb rig produced near-silence (peak {peak})"
    );
}
