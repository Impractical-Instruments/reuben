//! End-to-end stereo proof on the shipped example (ADR-0026): load `stereo-autopan.json`,
//! play a note, and confirm the engine serves two interleaved channels whose content differs
//! over time (the LFO is sweeping the voice across the field).

use std::path::PathBuf;

use reuben_core::{load, Arg, AudioConfig, Message, Plan};
use reuben_native::Engine;

fn instruments_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../instruments")
}

#[test]
fn stereo_autopan_plays_in_motion_across_two_channels() {
    let json = std::fs::read_to_string(instruments_dir().join("stereo-autopan.json"))
        .expect("read stereo-autopan.json");
    let graph = load(&json, &reuben_core::Registry::builtin()).expect("load");
    let plan = Plan::instantiate(graph, AudioConfig::new(48_000.0, 256)).expect("instantiate");
    assert_eq!(plan.config.channels, 2, "left+right taps -> stereo master");

    let mut engine = Engine::new(plan);
    assert_eq!(engine.channels(), 2);
    engine.queue(Message::new(
        "/voicer/note",
        [Arg::Float(69.0), Arg::Float(1.0)],
        0,
    ));

    // ~0.5 s of interleaved stereo. The autopan LFO runs at 0.5 Hz, so half a second sweeps
    // a quarter cycle — plenty for the L/R balance to shift.
    let frames = 24_000;
    let mut out = vec![0.0f32; frames * 2];
    engine.fill(&mut out);

    let left: Vec<f32> = out.iter().step_by(2).copied().collect();
    let right: Vec<f32> = out.iter().skip(1).step_by(2).copied().collect();
    let peak = |b: &[f32]| b.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(peak(&left) > 0.01, "left channel should carry signal");
    assert!(peak(&right) > 0.01, "right channel should carry signal");

    // The pan moves: the L/R energy balance early in the buffer differs from late in it.
    let half = left.len() / 2;
    let energy = |b: &[f32]| b.iter().map(|s| s * s).sum::<f32>().max(1e-12);
    let early = energy(&left[..half]) / energy(&right[..half]);
    let late = energy(&left[half..]) / energy(&right[half..]);
    assert!(
        (early - late).abs() / early > 0.05,
        "L/R balance should change over time (early ratio {early}, late {late})"
    );
}
