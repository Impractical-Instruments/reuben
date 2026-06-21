//! Integration: the JSON instrument format produces a working, deterministic rig, and the
//! committed schema stays in sync with the operator descriptors.

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::{load, AudioConfig, Graph, InstrumentDoc, Registry};

const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");
const COMMITTED_SCHEMA: &str = include_str!("../schema/instrument.schema.json");

/// Render `seconds` of `graph`, holding note A4 (MIDI 69) from frame 0.
fn render(graph: Graph, cfg: AudioConfig, seconds: f32) -> Vec<f32> {
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
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
fn default_instrument_loads_and_makes_a_440hz_tone() {
    let cfg = AudioConfig::new(48_000.0, 256);
    let graph = load(DEFAULT_JSON, &Registry::builtin()).expect("load default.json");
    let out = render(graph, cfg, 1.0);

    let peak = out.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    assert!(
        peak > 0.05,
        "loaded rig produced near-silence (peak {peak})"
    );

    // Fundamental ~440 Hz over the steady portion (skip the 0.1 s attack).
    let skip = (cfg.sample_rate * 0.1) as usize;
    let mut crossings = 0usize;
    let mut prev = 0.0f32;
    for &s in &out[skip..] {
        if prev <= 0.0 && s > 0.0 {
            crossings += 1;
        }
        prev = s;
    }
    let expected = (440.0 * 0.9) as usize;
    assert!(
        (expected - 20..=expected + 20).contains(&crossings),
        "expected ~{expected} crossings, got {crossings}"
    );
}

#[test]
fn save_then_reload_renders_identically() {
    // load -> save (from_graph) -> reload must render bit-identical output.
    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let g1 = load(DEFAULT_JSON, &reg).expect("load");
    let saved = InstrumentDoc::from_graph(&g1, "default");
    let g2 = saved.build(&reg).expect("rebuild from saved doc");

    let a = render(g1, cfg, 0.5);
    let b = render(g2, cfg, 0.5);
    assert_eq!(a.len(), b.len());
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "differ at sample {i}");
    }
}

#[test]
fn committed_schema_is_in_sync() {
    let fresh = reuben_core::schema::generate_pretty(&Registry::builtin());
    assert_eq!(
        COMMITTED_SCHEMA, fresh,
        "schema/instrument.schema.json is stale — run `cargo run -p reuben-core --example gen_schema`"
    );
}
