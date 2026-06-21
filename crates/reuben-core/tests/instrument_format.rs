//! Integration: the JSON instrument format produces a working, deterministic rig, and the
//! committed schema stays in sync with the operator descriptors.

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::{load, AudioConfig, Graph, InstrumentDoc, Registry};

const DEFAULT_JSON: &str = include_str!("../../../instruments/default.json");
const METRONOME_JSON: &str = include_str!("../../../instruments/metronome.json");
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

/// Render `seconds` of `graph`, holding every MIDI note in `midis` from frame 0.
fn render_notes(graph: Graph, cfg: AudioConfig, seconds: f32, midis: &[f32]) -> Vec<f32> {
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut all = Vec::with_capacity(blocks * cfg.block_size);
    for b in 0..blocks {
        let msgs: Vec<Message> = if b == 0 {
            midis
                .iter()
                .map(|&m| Message::new("/voicer/note", [Arg::Float(m), Arg::Float(1.0)], 0))
                .collect()
        } else {
            Vec::new()
        };
        r.render_block(&mut plan, &msgs, &mut buf);
        all.extend_from_slice(&buf);
    }
    all
}

fn rms(buf: &[f32]) -> f32 {
    (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt()
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
fn plays_a_chord_polyphonically() {
    // A C-major triad: three notes sounding at once exercises per-Voice fan-out and the
    // Lane-summing master tap. A single note uses one Voice; the triad uses three, so it
    // carries clearly more energy, and it must stay deterministic.
    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();
    let chord = [60.0, 64.0, 67.0];

    let single = render_notes(load(DEFAULT_JSON, &reg).unwrap(), cfg, 0.5, &chord[..1]);
    let triad = render_notes(load(DEFAULT_JSON, &reg).unwrap(), cfg, 0.5, &chord);

    // Past the attack, three voices sum to more energy than one.
    let win =
        |b: &[f32]| rms(&b[(cfg.sample_rate as usize / 5)..(cfg.sample_rate as usize / 5 + 4096)]);
    assert!(win(&triad) > 0.05, "triad near-silent");
    assert!(
        win(&triad) > win(&single) * 1.3,
        "triad ({}) should carry more energy than a single note ({})",
        win(&triad),
        win(&single)
    );

    // Determinism holds with polyphony.
    let again = render_notes(load(DEFAULT_JSON, &reg).unwrap(), cfg, 0.5, &chord);
    assert_eq!(triad.len(), again.len());
    for (i, (x, y)) in triad.iter().zip(&again).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "non-deterministic at sample {i}");
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

#[test]
fn clock_makes_a_sample_accurate_metronome() {
    // The metronome rig (Clock beat-gate -> plucked envelope -> tone) clicks on every beat
    // with no external input: a click fires right after each beat boundary and the gap
    // between beats is silent. Beats are on the sample grid (no drift), and it's
    // deterministic.
    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let render_clicks = |seconds: f32| -> Vec<f32> {
        let graph = load(METRONOME_JSON, &reg).expect("load metronome.json");
        let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
        let mut r = Renderer::new(&plan);
        let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
        let mut buf = vec![0.0f32; cfg.block_size];
        let mut all = Vec::with_capacity(blocks * cfg.block_size);
        for _ in 0..blocks {
            r.render_block(&mut plan, &[], &mut buf);
            all.extend_from_slice(&buf);
        }
        all
    };

    let out = render_clicks(2.0);
    let spb = 24_000usize; // 120 BPM @ 48 kHz

    let peak = |w: &[f32]| w.iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    for beat in 0..4 {
        let b = beat * spb;
        // A click fires in the window right after the beat boundary.
        let click_end = (b + 2_400).min(out.len());
        assert!(
            peak(&out[b..click_end]) > 0.05,
            "no click at beat {beat} (sample {b})"
        );
        // The remainder of the beat (sustain is 0) is silent.
        let gap = (b + 12_000)..(b + spb).min(out.len());
        if gap.start < gap.end {
            assert!(
                peak(&out[gap]) < 0.01,
                "beat {beat} should be silent before the next beat"
            );
        }
    }

    // Determinism holds for internally-clocked timing.
    let again = render_clicks(2.0);
    assert_eq!(out.len(), again.len());
    for (i, (x, y)) in out.iter().zip(&again).enumerate() {
        assert_eq!(x.to_bits(), y.to_bits(), "non-deterministic at sample {i}");
    }
}
