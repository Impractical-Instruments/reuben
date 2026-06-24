//! Stereo master + `pan` op (ADR-0026): channel-pinned taps land on separate logical
//! channels, master width is derived from the instrument (floor 2), and a fully-broadcast
//! (mono) instrument stays bit-identical to the pre-stereo single buffer.

use reuben_core::format::load;
use reuben_core::registry::Registry;
use reuben_core::AudioConfig;
use reuben_core::{Plan, Renderer};

fn reg() -> Registry {
    Registry::builtin()
}

fn peak(buf: &[f32]) -> f32 {
    buf.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
}

/// An oscillator panned hard-left, tapped left→channel 0 / right→channel 1.
const HARD_LEFT: &str = r#"
{
  "instrument": "stereo-test",
  "nodes": [
    { "type": "oscillator", "address": "/osc", "params": { "freq": 220.0 } },
    { "type": "pan", "address": "/pan", "params": { "pan": -1.0 } }
  ],
  "connections": [
    { "from": {"node":"/osc","port":"audio"}, "to": {"node":"/pan","port":"audio"} }
  ],
  "outputs": [
    { "node": "/pan", "port": "left",  "channel": 0 },
    { "node": "/pan", "port": "right", "channel": 1 }
  ]
}"#;

#[test]
fn channel_taps_route_to_separate_channels() {
    let g = load(HARD_LEFT, &reg()).expect("load");
    let cfg = AudioConfig::new(48_000.0, 256);
    let mut plan = Plan::instantiate(g, cfg).expect("instantiate");
    assert_eq!(
        plan.config.channels, 2,
        "two channels referenced -> width 2"
    );

    let mut r = Renderer::new(&plan);
    let mut master = vec![vec![0.0f32; 256], vec![0.0f32; 256]];
    // Warm up a couple blocks so the oscillator is well into its waveform.
    for _ in 0..4 {
        r.render_block_multi(&mut plan, &[], &mut master);
    }
    assert!(
        peak(&master[0]) > 0.1,
        "hard-left: channel 0 should carry the signal"
    );
    assert!(
        peak(&master[1]) < 1e-4,
        "hard-left: channel 1 should be silent, peak {}",
        peak(&master[1])
    );
}

#[test]
fn master_width_floors_to_stereo_for_a_mono_instrument() {
    // No channel indices anywhere (all broadcast) -> still presents two channels.
    let json = r#"{
      "instrument": "mono",
      "nodes": [{ "type": "oscillator", "address": "/osc", "params": { "freq": 220.0 } }],
      "outputs": [{ "node": "/osc", "port": "audio" }]
    }"#;
    let g = load(json, &reg()).expect("load");
    let plan = Plan::instantiate(g, AudioConfig::new(48_000.0, 128)).expect("instantiate");
    assert_eq!(plan.config.channels, 2);
}

#[test]
fn broadcast_instrument_is_bit_identical_mono_and_across_channels() {
    // A broadcast (mono) instrument: mono render_block == every channel of the multi render,
    // sample-for-sample. This is the ADR-0026 backwards-compat guarantee.
    let json = r#"{
      "instrument": "mono",
      "nodes": [
        { "type": "oscillator", "address": "/osc", "params": { "freq": 330.0 } },
        { "type": "output", "address": "/out" }
      ],
      "connections": [
        { "from": {"node":"/osc","port":"audio"}, "to": {"node":"/out","port":"audio"} }
      ],
      "outputs": [{ "node": "/out", "port": "audio" }]
    }"#;
    let cfg = AudioConfig::new(48_000.0, 128);

    let mut plan_a = Plan::instantiate(load(json, &reg()).unwrap(), cfg).unwrap();
    let mut ra = Renderer::new(&plan_a);
    let mut mono = vec![0.0f32; 128];

    let mut plan_b = Plan::instantiate(load(json, &reg()).unwrap(), cfg).unwrap();
    let mut rb = Renderer::new(&plan_b);
    let mut master = vec![vec![0.0f32; 128], vec![0.0f32; 128]];

    for _ in 0..3 {
        ra.render_block(&mut plan_a, &[], &mut mono);
        rb.render_block_multi(&mut plan_b, &[], &mut master);
        for i in 0..128 {
            assert_eq!(
                mono[i].to_bits(),
                master[0][i].to_bits(),
                "ch0 != mono at {i}"
            );
            assert_eq!(
                master[0][i].to_bits(),
                master[1][i].to_bits(),
                "broadcast channels must be identical at {i}"
            );
        }
    }
}
