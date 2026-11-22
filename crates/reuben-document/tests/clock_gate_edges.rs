//! The Clock re-emits `gate`'s current level at frame 0 of every block. A downstream edge detector
//! must see exactly one rising edge per beat whatever the block size: the re-emit of an
//! already-high level is not a trigger.

use reuben_core::graph::Graph;
use reuben_core::message::{Arg, Message};
use reuben_core::operators::{clock, envelope, euclid, output};
use reuben_core::operators::{Clock, Envelope, Euclid, OscOut, Output};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::AudioConfig;

const SR: f32 = 48_000.0;
/// 240 BPM: a beat every 12000 frames, the gate high for the first 6000.
const BEAT: usize = 12_000;
const TOTAL: usize = 4 * BEAT;

/// `/clk.gate` drives a one-step, one-pulse `euclid` (a gate-on per rising clock edge, reported
/// through `/fb`) and an `envelope` whose `cv` is the master output.
fn rig() -> Graph {
    let mut g = Graph::new();
    let clk = g.add("/clk", Clock::new());
    g.set_value(clk, "tempo", &Arg::F32(240.0));
    let eu = g.add("/eu", Euclid::new());
    g.set_value(eu, "steps", &Arg::I32(1));
    g.set_value(eu, "pulses", &Arg::I32(1));
    g.connect(clk, clock::OUT_GATE, eu, euclid::IN_CLOCK);
    let fb = g.add("/fb", OscOut::new());
    g.connect(eu, euclid::OUT_GATE, fb, 0);
    let env = g.add("/env", Envelope::new());
    for (name, v) in [
        ("attack", 0.001),
        ("decay", 0.001),
        ("sustain", 0.5),
        ("release", 0.001),
    ] {
        g.set_value(env, name, &Arg::F32(v));
    }
    g.connect(clk, clock::OUT_GATE, env, envelope::IN_GATE);
    let out = g.add("/out", Output::new());
    g.connect(env, envelope::OUT_CV, out, output::IN_AUDIO);
    g.tap_output(out, output::OUT_AUDIO);
    g
}

/// Render [`TOTAL`] frames in blocks of `block`. Returns the global frames of each `euclid`
/// gate-on and the master `cv`.
fn run(block: usize) -> (Vec<usize>, Vec<f32>) {
    let mut plan = Plan::instantiate(rig(), AudioConfig::new(SR, block)).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut master = vec![vec![0.0f32; block]; plan.config.channels];
    let mut outbound: Vec<Message> = Vec::new();
    let (mut ons, mut cv) = (Vec::new(), Vec::new());
    let mut at = 0;
    while at < TOTAL {
        outbound.clear();
        r.render_block_multi(&mut plan, &[], &[], &mut master, &mut outbound);
        for m in outbound.iter().filter(|m| m.address == "/fb") {
            if m.arg == Arg::F32(1.0) && at + m.frame < TOTAL {
                ons.push(at + m.frame);
            }
        }
        cv.extend_from_slice(&master[0][..block]);
        at += block;
    }
    cv.truncate(TOTAL);
    (ons, cv)
}

#[test]
fn the_frame_zero_gate_reemit_adds_no_trigger_at_any_block_size() {
    let (want_ons, want_cv) = run(1);
    for (k, &on) in want_ons.iter().enumerate() {
        assert!(on.abs_diff(k * BEAT) <= 1, "gate-on {k} at {on}");
    }
    assert_eq!(want_ons.len(), 4, "one gate-on per beat: {want_ons:?}");
    // The envelope reaches its peak only in the attack right after a rising edge; a spurious
    // re-trigger mid-beat would re-attack from sustain toward 1.0.
    for (i, &v) in want_cv.iter().enumerate() {
        if v > 0.6 {
            assert!(i % BEAT < 200, "envelope re-attacked at {i} (level {v})");
        }
    }
    for block in [7usize, 64, 128, 333] {
        let (ons, cv) = run(block);
        assert_eq!(ons, want_ons, "gate-ons, block {block}");
        assert!(
            cv.iter()
                .zip(&want_cv)
                .all(|(a, b)| a.to_bits() == b.to_bits()),
            "envelope contour, block {block}"
        );
    }
}
