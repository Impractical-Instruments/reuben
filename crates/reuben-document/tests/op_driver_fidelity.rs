//! **Behavioral / end-to-end fidelity pin** for `OpDriver`: the harness's output must match the
//! real `load` -> `instantiate` -> `render_block` path sample-for-sample.
//!
//! It needs both halves — `OpDriver` from the render crate (behind its `bench` feature) and the
//! loader from this one — so it lives here rather than beside the harness. That feature is why the
//! target declares `required-features = ["bench"]`: a bare `cargo test` **skips** this pin. CI runs
//! it (`--features reuben-document/bench`); locally, name the feature or you are not running it.

use reuben_core::op_driver::OpDriver;
use reuben_core::operators::{oscillator, Oscillator};
use reuben_core::render::Renderer;
use reuben_core::{AudioConfig, Plan, Registry};

const BLOCK_SIZE: usize = 128;

#[test]
fn op_driver_output_matches_the_real_render_path_sample_exact() {
    const SR: f32 = 48_000.0;
    const FREQ: f32 = 440.0;
    const BLOCKS: usize = 4;
    const N: usize = BLOCKS * BLOCK_SIZE;

    let mut d = OpDriver::for_type(Oscillator::new(), SR);
    d.set(oscillator::IN_FREQ, FREQ);
    d.render(N);
    let driver_out = d.output(oscillator::OUT_AUDIO).to_vec();

    // --- Real side: the same oscillator as a one-node unity-passthrough instrument, driven
    // through the production `load` → `instantiate` → `render_block` path. `/out` is `output`,
    // a sample-exact unity passthrough, so the master tap equals the oscillator's raw output. ---
    let json = r#"{
        "instrument": "behavioral_pin_osc",
        "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": 440.0 } },
            { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/osc" } } }
        ],
        "outputs": [ { "node": "/out", "port": "audio" } ]
    }"#;
    let graph = reuben_document::load(json, &Registry::builtin()).expect("instrument loads");
    let config = AudioConfig::new(SR, BLOCK_SIZE);
    let mut plan = Plan::instantiate(graph, config).expect("instrument instantiates");
    let mut renderer = Renderer::new(&plan);
    let mut block = vec![0.0f32; BLOCK_SIZE];
    let mut real_out = Vec::with_capacity(N);
    for _ in 0..BLOCKS {
        // No messages: `freq` is a held Value, materialized identically on both paths.
        renderer.render_block(&mut plan, &[], &mut block);
        real_out.extend_from_slice(&block);
    }

    assert_eq!(driver_out.len(), N);
    assert_eq!(real_out.len(), N);
    // Bit-exact: both paths run identical DSP on identical per-node seeding at the same block
    // geometry. Any divergence between the driver's `step_node` and production `process_node`
    // stepping — the drift this pin guards — shows up as a sample mismatch.
    assert_eq!(
        driver_out, real_out,
        "OpDriver output diverged from the real render path"
    );
    // Guard against the degenerate pass where both sides are silent (e.g. a future change that
    // zeroes the oscillator): a 440 Hz tone must actually be present.
    let peak = real_out.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(
        peak > 0.05,
        "expected an audible tone, got near-silence (peak {peak})"
    );
}
