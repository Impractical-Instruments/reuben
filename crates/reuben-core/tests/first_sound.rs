//! Integration: the MVP rig makes a verifiable, deterministic sound.
//!
//! Rig: Voicer -> Oscillator -> Filter -> VCA(mul) -> Output, with the VCA gain driven by
//! an Envelope -> Power (exponential-style volume curve, ADR-0027); a single held note (A4)
//! is sent at frame 0. This exercises the whole spine end-to-end: message routing, the
//! per-block topo schedule, Signal edges (incl. freq/gate CV), block-slicing and the master tap.

use reuben_core::graph::{Graph, NodeKey};
use reuben_core::message::{Arg, Message};
use reuben_core::operators::{envelope, mul, oscillator, output, power, voicer};
use reuben_core::operators::{Envelope, Filter, Mul, Oscillator, Output, Power, Voicer};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::AudioConfig;

/// Build the standard first-sound rig. Returns the graph and the voicer key (so the
/// caller knows the note address is "/voicer").
fn build_rig() -> Graph {
    let mut g = Graph::new();
    let v: NodeKey = g.add("/voicer", Voicer::new());
    let osc = g.add("/osc", Oscillator::new());
    let filt = g.add("/filter", Filter::new());
    let env = g.add("/env", Envelope::new());
    let curve = g.add("/env_curve", Power::new());
    let vca = g.add("/env_vca", Mul::new());
    let out = g.add("/out", Output::new());

    g.connect(v, voicer::OUT_FREQ, osc, oscillator::IN_FREQ);
    g.connect(osc, oscillator::OUT_AUDIO, filt, 0);
    // VCA: filtered audio * shaped envelope CV (env -> power -> mul).
    g.connect(filt, 0, vca, mul::IN_A);
    g.connect(v, voicer::OUT_GATE, env, envelope::IN_GATE);
    g.connect(env, envelope::OUT_CV, curve, power::IN_X);
    g.connect(curve, power::OUT_OUT, vca, mul::IN_B);
    g.connect(vca, mul::OUT_OUT, out, output::IN_AUDIO);
    g.tap_output(out, output::OUT_AUDIO);

    g.set_param(filt, "cutoff", 3_000.0);
    g
}

/// Render `seconds` of the rig, holding note A4 (MIDI 69) from frame 0. Returns the full
/// interleaved (mono) output buffer.
fn render_rig(cfg: AudioConfig, seconds: f32) -> Vec<f32> {
    let mut plan = Plan::instantiate(build_rig(), cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut all = Vec::with_capacity(blocks * cfg.block_size);
    for b in 0..blocks {
        let msgs: Vec<Message> = if b == 0 {
            vec![Message::new(
                "/voicer/note",
                [Arg::Float(69.0), Arg::Float(1.0)],
                0,
            )]
        } else {
            Vec::new()
        };
        r.render_block(&mut plan, &msgs, &mut buf);
        all.extend_from_slice(&buf);
    }
    all
}

#[test]
fn rig_makes_a_non_silent_tone_at_440hz() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let out = render_rig(cfg, 1.0);

    // Non-silent.
    let peak = out.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(peak > 0.05, "rig produced near-silence (peak {peak})");

    // Fundamental ~440 Hz: count upward zero crossings over the steady portion
    // (skip the first 0.1 s so the attack ramp doesn't perturb the count).
    let skip = (cfg.sample_rate * 0.1) as usize;
    let mut crossings = 0usize;
    let mut prev = 0.0f32;
    for &s in &out[skip..] {
        if prev <= 0.0 && s > 0.0 {
            crossings += 1;
        }
        prev = s;
    }
    // ~440 crossings over ~0.9 s.
    let expected = (440.0 * 0.9) as usize;
    let lo = expected - 20;
    let hi = expected + 20;
    assert!(
        (lo..=hi).contains(&crossings),
        "expected ~{expected} crossings over the steady portion, got {crossings}"
    );
}

#[test]
fn envelope_attack_is_audible() {
    // The amplitude early in the note (during attack) should be lower than once the
    // envelope has opened — proves the gate/envelope path is wired and time-varying.
    let cfg = AudioConfig::new(48_000.0, 256);
    let out = render_rig(cfg, 0.5);

    // Very start of the note: the envelope has barely opened, so amplitude is tiny.
    let early_rms = rms(&out[..128]);
    // Well into the sustain (past attack+decay): the note is at full body.
    let later = (cfg.sample_rate * 0.2) as usize;
    let later_rms = rms(&out[later..later + 1_024]);

    assert!(
        later_rms > early_rms * 3.0,
        "expected the note to swell after attack (early {early_rms}, later {later_rms})"
    );
}

#[test]
fn render_is_deterministic() {
    // The determinism invariant (ADR-0001): re-rendering the same rig with the same
    // input yields bit-identical output. (Serial today; the contract must hold when a
    // parallel executor slots in behind the same trait.)
    let cfg = AudioConfig::new(48_000.0, 256);
    let a = render_rig(cfg, 0.5);
    let b = render_rig(cfg, 0.5);
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(
            x.to_bits(),
            y.to_bits(),
            "non-deterministic at sample {i}: {x} vs {y}"
        );
    }
}

fn rms(buf: &[f32]) -> f32 {
    let sum: f32 = buf.iter().map(|x| x * x).sum();
    (sum / buf.len() as f32).sqrt()
}
