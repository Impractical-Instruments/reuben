//! first_sound — render the MVP audio spine to a WAV file so you can hear it.
//!
//! Rig: Oscillator -> Filter -> VCA(mul) -> Output, with the VCA gain driven by an
//! Envelope -> PowerF32Signal (exponential-style volume curve, ADR-0027). `osc.freq` defaults to
//! 440 Hz; `env.gate` is a held Value raised to `1.0` at frame 0.
//!
//! Run: `cargo run -p reuben-core --example first_sound` -> `first_sound.wav`.

use reuben_core::graph::Graph;
use reuben_core::message::Message;
use reuben_core::operators::{envelope, mul, oscillator, output, power};
use reuben_core::operators::{Envelope, Filter, MulF32Signal, Oscillator, Output, PowerF32Signal};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::AudioConfig;

fn main() {
    let cfg = AudioConfig::new(48_000.0, 256);

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
    g.connect(env, envelope::OUT_CV, curve, power::IN_X);
    g.connect(curve, power::OUT_OUT, vca, mul::mul_f32_signal::IN_B);
    g.connect(vca, mul::mul_f32_signal::OUT_OUT, out, output::IN_AUDIO);
    g.tap_output(out, output::OUT_AUDIO);

    g.set_param(filt, "cutoff", 3_000.0);

    let mut plan = Plan::instantiate(g, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: cfg.sample_rate as u32,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create("first_sound.wav", spec).expect("create wav");

    let seconds = 2.0;
    let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    for b in 0..blocks {
        // Open the held envelope gate (Value `1.0`) at the very start; held for the rest.
        let msgs: Vec<Message> = if b == 0 {
            vec![Message::float("/env/gate", 1.0, 0)]
        } else {
            Vec::new()
        };
        r.render_block(&mut plan, &msgs, &mut buf);
        for &s in &buf {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            writer.write_sample(v).expect("write sample");
        }
    }
    writer.finalize().expect("finalize wav");
    println!("wrote first_sound.wav ({blocks} blocks)");
}
