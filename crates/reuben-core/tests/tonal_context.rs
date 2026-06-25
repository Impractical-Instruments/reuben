//! Integration: the tonal-context bus end-to-end (ADR-0013, ADR-0015) — a context node
//! publishes the latched key/scale, the engine routes and slices it onto downstream readers,
//! and a Voicer resolves degree notes to Hz through it. Exercises the engine plumbing the
//! operator unit tests can't: the context arena, the third route lane, and sample-accurate
//! re-slicing on a context change.

use reuben_core::message::{Arg, Message};
use reuben_core::operators::{ContextOp, Snap, Voicer};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::vocab::harmony::Harmony;
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load, AudioConfig, Graph, Registry};

const SCALE_DEMO: &str = include_str!("../../../instruments/scale-demo.json");
const AUTOTUNE: &str = include_str!("../../../instruments/autotune.json");

const CFG: AudioConfig = AudioConfig {
    sample_rate: 48_000.0,
    block_size: 256,
    channels: AudioConfig::MIN_CHANNELS,
};

fn hz(midi: f32) -> f32 {
    Harmony::default().hz(Pitch::from_midi(midi))
}

/// A minimal `context -> voicer(mono)` rig, tapping the Voicer's `freq` so a test can read
/// the resolved pitch directly. Port indices: ContextOp `ctx` out = 0; Voicer `notes` in = 0,
/// `ctx` in = 1, `freq` out = 0.
fn context_voicer() -> Graph {
    let mut g = Graph::new();
    let c = g.add("/context", ContextOp::new());
    let v = g.add("/voicer", Voicer::new());
    g.set_param(v, "voices", 1.0);
    g.connect(c, 0, v, 1);
    g.tap_output(v, 0);
    g
}

#[test]
fn degree_note_resolves_then_respells_across_blocks() {
    let mut plan = Plan::instantiate(context_voicer(), CFG).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    // Block 1: play degree 2. In C major that is E (MIDI 64).
    let note = Message::new("/voicer/notes", Note::new(Pitch::Degree(2), 1.0), 0);
    r.render_block(&mut plan, &[note], &mut buf);
    approx::assert_relative_eq!(buf[CFG.block_size - 1], hz(64.0), epsilon = 1e-2);

    // Block 2: move the root to D (62) with no new note. The held degree 2 re-spells to F♯
    // (66) — proof the latched context drives a live re-spell through the engine, not just
    // the operator.
    let key = Message::new("/context/root", Arg::F32(62.0), 0);
    r.render_block(&mut plan, &[key], &mut buf);
    approx::assert_relative_eq!(buf[CFG.block_size - 1], hz(66.0), epsilon = 1e-2);
}

#[test]
fn context_change_mid_block_is_sample_accurate() {
    let mut plan = Plan::instantiate(context_voicer(), CFG).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    // Hold degree 2 (E in C major).
    r.render_block(
        &mut plan,
        &[Message::new(
            "/voicer/notes",
            Note::new(Pitch::Degree(2), 1.0),
            0,
        )],
        &mut buf,
    );

    // In the next block, change the root to D at frame 128. The change slices the block: the
    // first half still resolves in C major (E), the second half in D major (F♯) — one
    // sample-accurate timeline, no block-quantization internally.
    let key = Message::new("/context/root", Arg::F32(62.0), 128);
    r.render_block(&mut plan, &[key], &mut buf);
    approx::assert_relative_eq!(buf[100], hz(64.0), epsilon = 1e-2); // before 128 → E
    approx::assert_relative_eq!(buf[200], hz(66.0), epsilon = 1e-2); // after 128 → F♯
}

#[test]
fn snap_quantizes_an_off_key_gesture() {
    // context -> snap -> voicer. Play D♯ (63), an off-scale pitch; the snap pulls it to the
    // nearest C-major tone (D, 62, on the down tie-break) before the Voicer resolves it.
    let mut g = Graph::new();
    let c = g.add("/context", ContextOp::new());
    let s = g.add("/snap", Snap::new()); // inputs: notes=0, ctx=1; output degrees=0
    let v = g.add("/voicer", Voicer::new());
    g.set_param(v, "voices", 1.0);
    g.connect(c, 0, s, 1); // context.ctx -> snap.ctx
    g.connect(c, 0, v, 1); // context.ctx -> voicer.ctx
    g.connect(s, 0, v, 0); // snap.degrees -> voicer.notes
    g.tap_output(v, 0);

    let mut plan = Plan::instantiate(g, CFG).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];
    let note = Message::new("/snap/notes", Note::new(Pitch::Absolute(63.0), 1.0), 0);
    r.render_block(&mut plan, &[note], &mut buf);
    approx::assert_relative_eq!(buf[CFG.block_size - 1], hz(62.0), epsilon = 1e-2);
    // D
}

#[test]
fn demo_instruments_load_and_play() {
    let reg = Registry::builtin();
    for json in [SCALE_DEMO, AUTOTUNE] {
        let graph = load(json, &reg).expect("load demo instrument");
        let mut plan = Plan::instantiate(graph, CFG).expect("instantiate");
        let mut r = Renderer::new(&plan);
        let mut buf = vec![0.0f32; CFG.block_size];
        // scale-demo self-plays; autotune needs input — drive both so each makes sound.
        let note = Message::new("/snap/notes", Note::new(Pitch::Absolute(67.3), 1.0), 0);
        let mut peak = 0.0f32;
        for _ in 0..400 {
            r.render_block(&mut plan, std::slice::from_ref(&note), &mut buf);
            peak = peak.max(buf.iter().fold(0.0, |m, &s| m.max(s.abs())));
        }
        assert!(peak > 0.01, "demo instrument produced no sound");
    }
}
