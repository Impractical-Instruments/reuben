//! Integration: the MVP audio spine makes a verifiable, deterministic sound.
//!
//! Rig: Oscillator -> Filter -> VCA(mul) -> Output, with the VCA gain driven by an
//! Envelope -> PowerF32Signal (exponential-style volume curve, ADR-0027). `osc.freq` defaults to
//! 440 Hz (an `f32_buffer` materialized from its meta) and `env.gate` is a held **Value** raised to
//! `1.0` at frame 0 via a routed message. This is the spine the ADR-0031 Value/Signal flip churns —
//! message routing, the per-block topo schedule, Signal edges, held-value block-slicing, the master
//! tap. (Polyphonic note allocation through a hosted `voicer` is covered by `voicer_host.rs`.)

use reuben_core::graph::Graph;
use reuben_core::message::Message;
use reuben_core::operators::{envelope, mul, oscillator, output, power};
use reuben_core::operators::{Envelope, Filter, MulF32Signal, Oscillator, Output, PowerF32Signal};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::AudioConfig;

/// Build the first-sound audio spine (one voice's worth of synth chain): a 440 Hz oscillator through
/// a lowpass, amplitude-shaped by an envelope whose `gate` a caller raises via a routed message.
fn build_rig() -> Graph {
    let mut g = Graph::new();
    let osc = g.add("/osc", Oscillator::new());
    let filt = g.add("/filter", Filter::new());
    let env = g.add("/env", Envelope::new());
    let curve = g.add("/env_curve", PowerF32Signal::new());
    let vca = g.add("/env_vca", MulF32Signal::new());
    let out = g.add("/out", Output::new());

    // `osc.freq` is left unwired — it materializes 440 Hz from its meta default.
    g.connect(osc, oscillator::OUT_AUDIO, filt, 0);
    // VCA: filtered audio * shaped envelope CV (env -> power -> mul).
    g.connect(filt, 0, vca, mul::mul_f32_signal::IN_A);
    g.connect(env, envelope::OUT_CV, curve, power::power_f32_signal::IN_X);
    g.connect(
        curve,
        power::power_f32_signal::OUT_OUT,
        vca,
        mul::mul_f32_signal::IN_B,
    );
    g.connect(vca, mul::mul_f32_signal::OUT_OUT, out, output::IN_AUDIO);
    g.tap_output(out, output::OUT_AUDIO);

    g.set_param(filt, "cutoff", 3_000.0);
    g
}

/// Render `seconds` of the rig, holding the envelope gate open (Value `1.0`) from frame 0. Returns
/// the full (mono) output buffer.
fn render_rig(cfg: AudioConfig, seconds: f32) -> Vec<f32> {
    let mut plan = Plan::instantiate(build_rig(), cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut all = Vec::with_capacity(blocks * cfg.block_size);
    for b in 0..blocks {
        // The gate is a held Value: raise it once at frame 0; the latch holds it across blocks.
        let msgs: Vec<Message> = if b == 0 {
            vec![Message::float("/env/gate", 1.0, 0)]
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
