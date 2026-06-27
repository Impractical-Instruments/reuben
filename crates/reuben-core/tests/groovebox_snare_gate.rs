//! Diagnostic: observe the actual snare gate of instruments/groovebox.json.
//!
//! The snare-noise envelope (sustain=0) "seems to stay open." The envelope itself is proven
//! correct (see operators::envelope unit tests), so the suspect is the gate driving it. This
//! renders the live instrument with the master tap re-pointed at `/snare_v gate` and reports
//! the real pulse train: a clean drum gate is a short pulse per hit, NOT a held/retriggering level.

use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::{load, AudioConfig, Registry};

const GROOVEBOX: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../instruments/groovebox.json"
));

/// Load groovebox with its master output re-pointed at `node`'s `port` (so the rendered
/// buffer IS that probe signal, not the drum mix).
fn render_probe(node: &str, port: &str, seconds: f32) -> (Vec<f32>, AudioConfig) {
    let mut doc: serde_json::Value = serde_json::from_str(GROOVEBOX).expect("parse groovebox");
    doc["outputs"] = serde_json::json!([{ "node": node, "port": port }]);
    let json = doc.to_string();

    let cfg = AudioConfig::new(48_000.0, 256);
    let graph = load(&json, &Registry::builtin()).expect("load groovebox probe");
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);

    let blocks = (cfg.sample_rate * seconds) as usize / cfg.block_size;
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut all = Vec::with_capacity(blocks * cfg.block_size);
    let no_msgs: Vec<Message> = Vec::new();
    for _ in 0..blocks {
        r.render_block(&mut plan, &no_msgs, &mut buf);
        all.extend_from_slice(&buf);
    }
    (all, cfg)
}

/// (rising, falling) edge sample indices of a 0/1 gate.
fn edges(sig: &[f32]) -> (Vec<usize>, Vec<usize>) {
    let (mut rise, mut fall) = (Vec::new(), Vec::new());
    let mut prev = 0.0f32;
    for (i, &s) in sig.iter().enumerate() {
        if prev < 0.5 && s >= 0.5 {
            rise.push(i);
        } else if prev >= 0.5 && s < 0.5 {
            fall.push(i);
        }
        prev = s;
    }
    (rise, fall)
}

#[test]
#[ignore = "ADR-0032 follow-up: depends on a voicer instrument / voicer.freq tap; re-author to the hosted-voice model, then restore"]
fn report_snare_gate_shape() {
    // 2 bars at 120 BPM (default tempo) = 4 s. Snare default pattern hits step5 and step13.
    let (gate, cfg) = render_probe("/snare_v", "gate", 4.0);
    let sr = cfg.sample_rate;

    let high = gate.iter().filter(|&&s| s >= 0.5).count();
    let frac = high as f32 / gate.len() as f32;
    let (rise, fall) = edges(&gate);

    eprintln!("--- /snare_v gate over 4 s @120BPM ---");
    eprintln!(
        "samples high: {high}/{} ({:.1}% duty)",
        gate.len(),
        frac * 100.0
    );
    eprintln!("rising edges ({}): {:?}", rise.len(), rise);
    eprintln!("falling edges ({}): {:?}", fall.len(), fall);
    let durs: Vec<f32> = rise
        .iter()
        .zip(&fall)
        .map(|(&r, &f)| (f.saturating_sub(r)) as f32 / sr * 1000.0)
        .collect();
    eprintln!("pulse durations (ms): {durs:?}");

    // A 16th at 120 BPM is 125 ms; a clean drum gate pulse is well under that and the gate
    // spends most of its time LOW. If this fails, the gate is stuck/retriggering — the rogue.
    assert!(
        frac < 0.5,
        "snare gate is high {:.0}% of the time — not a percussive pulse (rogue gate)",
        frac * 100.0
    );
}

fn rms(buf: &[f32]) -> f32 {
    if buf.is_empty() {
        return 0.0;
    }
    (buf.iter().map(|x| x * x).sum::<f32>() / buf.len() as f32).sqrt()
}

#[test]
#[ignore = "ADR-0032 follow-up: depends on a voicer instrument / voicer.freq tap; re-author to the hosted-voice model, then restore"]
fn report_snare_nenv_output_decays_between_hits() {
    // The enveloped noise itself: a hit at ~24000 (gate pulse 24000..27000), then nothing until
    // the next hit at 72000. With attack 1ms / decay 90ms / sustain 0 / release 60ms the sound
    // must be gone well before the gap, so a deep gap window is ~silent. If it isn't, *that* is
    // the "stays open" — energy sustaining where there should be none.
    // The enveloped noise audio is now the VCA output (env -> power -> mul), ADR-0027.
    let (env, cfg) = render_probe("/snare_nenv_vca", "out", 4.0);
    let sr = cfg.sample_rate as usize;

    let hit = rms(&env[24_000..30_000]); // the first snare snap (attack+decay+release)
    let gap = rms(&env[40_000..70_000]); // deep gap: should be silent
    eprintln!("--- /snare_nenv_vca out ---");
    eprintln!("hit RMS (0.5–0.625 s): {hit:.5}");
    eprintln!("gap RMS (0.83–1.46 s): {gap:.5}");
    eprintln!("gap/hit ratio: {:.4}", gap / hit.max(1e-9));
    // sanity: there IS a snap
    assert!(hit > 1e-3, "no snare snap rendered (hit RMS {hit})");
    // the suspect: is the gap actually silent?
    assert!(
        gap < hit * 0.05,
        "snare noise sustains in the gap (gap {gap} vs hit {hit}) — the envelope stays open"
    );
    let _ = sr;
}
