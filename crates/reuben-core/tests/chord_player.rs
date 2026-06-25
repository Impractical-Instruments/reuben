//! Integration: the V1.3 Chord player end-to-end (ADR-0022) — a `chord` op stacks scale-relative
//! thirds and emits `degree` notes, the engine routes them to a polyphonic Voicer, and the Voicer
//! resolves each degree through the tonal context. Exercises the engine plumbing the operator unit
//! tests can't: routing the chord op's emitted Messages to a real Voicer's voices, and a live chord
//! re-spell on a key change driven through the full graph.

use reuben_core::harmony::Harmony;
use reuben_core::message::{Arg, Message};
use reuben_core::operators::{Chord, ContextOp, Voicer};
use reuben_core::pitch::{Note, Pitch};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::{load, AudioConfig, Graph, Registry};

const CHORD_PLAYER: &str = include_str!("../../../instruments/chord-player.json");

const CFG: AudioConfig = AudioConfig {
    sample_rate: 48_000.0,
    block_size: 256,
    channels: AudioConfig::MIN_CHANNELS,
};

fn hz(midi: f32) -> f32 {
    Harmony::default().hz(Pitch::from_midi(midi))
}

/// A `context -> chord -> voicer(poly)` rig that taps a chosen Voicer output across all voices
/// (summed to the mono tap). Port indices: ContextOp `ctx` out = 0; Chord `set` in = 0,
/// `degrees` out = 0; Voicer `notes` in = 0, `ctx` in = 1, `freq` out = 0, `gate` out = 1.
fn chord_rig_tapping(voices: f32, tap_port: usize) -> Graph {
    let mut g = Graph::new();
    let c = g.add("/context", ContextOp::new());
    let ch = g.add("/chord", Chord::new());
    let v = g.add("/voicer", Voicer::new());
    g.set_param(v, "voices", voices);
    g.connect(ch, 0, v, 0); // chord.degrees -> voicer.notes
    g.connect(c, 0, v, 1); // context.ctx -> voicer.ctx
    g.tap_output(v, tap_port);
    g
}

/// Tap `freq` (the resolved chord-tone pitches) across all voices.
fn chord_rig(voices: f32) -> Graph {
    chord_rig_tapping(voices, 0)
}

#[test]
fn tapping_a_triad_sounds_three_chord_tones() {
    // Press chord root degree 0 (the I chord). The chord op emits degrees 0, 2, 4; the Voicer
    // (3 voices) resolves them in C major to C(60), E(64), G(67). Tapping voicer.freq sums the
    // three voices, so the mono tap is their sum — assert it equals C+E+G, proving all three
    // chord tones are sounding through the real engine (not just emitted).
    let mut plan = Plan::instantiate(chord_rig(3.0), CFG).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    let press = Message::new("/chord/set", Note::new(Pitch::Degree(0), 1.0), 0);
    r.render_block(&mut plan, &[press], &mut buf);

    let sum = buf[CFG.block_size - 1];
    let expected = hz(60.0) + hz(64.0) + hz(67.0); // C + E + G
    approx::assert_relative_eq!(sum, expected, epsilon = 1e-1);
}

#[test]
fn releasing_the_root_stops_all_chord_tones() {
    // Press then release the I chord across two blocks, tapping the Voicer's GATE: while held,
    // all three voices gate-on (sum 3.0); after release, every voice's gate falls to 0 — the
    // chord op's matched note-offs reach all three voices through the engine.
    let mut plan = Plan::instantiate(chord_rig_tapping(3.0, 1), CFG).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    r.render_block(
        &mut plan,
        &[Message::new(
            "/chord/set",
            Note::new(Pitch::Degree(0), 1.0),
            0,
        )],
        &mut buf,
    );
    approx::assert_relative_eq!(buf[CFG.block_size - 1], 3.0, epsilon = 1e-3); // 3 gates on

    r.render_block(
        &mut plan,
        &[Message::new(
            "/chord/set",
            Note::new(Pitch::Degree(0), 0.0),
            0,
        )],
        &mut buf,
    );
    approx::assert_relative_eq!(buf[CFG.block_size - 1], 0.0, epsilon = 1e-3); // all released
}

#[test]
fn held_chord_respells_live_on_a_key_change() {
    // Hold the I chord (C E G in C major). Then move the key root up a whole tone to D (62) with
    // no new press: the same held degrees 0,2,4 re-spell to D F# A through the context bus — the
    // signature behavior, proven end-to-end (chord op holds degrees; the Voicer + context do the
    // re-spell). New summed freq = D(62) + F#(66) + A(69).
    let mut plan = Plan::instantiate(chord_rig(3.0), CFG).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    r.render_block(
        &mut plan,
        &[Message::new(
            "/chord/set",
            Note::new(Pitch::Degree(0), 1.0),
            0,
        )],
        &mut buf,
    );
    approx::assert_relative_eq!(
        buf[CFG.block_size - 1],
        hz(60.0) + hz(64.0) + hz(67.0),
        epsilon = 1e-1
    );

    // Re-key to D (root 62), no new chord press.
    r.render_block(
        &mut plan,
        &[Message::new("/context/root", Arg::F32(62.0), 0)],
        &mut buf,
    );
    approx::assert_relative_eq!(
        buf[CFG.block_size - 1],
        hz(62.0) + hz(66.0) + hz(69.0), // D + F# + A
        epsilon = 1e-1
    );
}

#[test]
fn two_overlapping_chords_release_independently() {
    // Press I (0,2,4) then IV (3,5,7), 6 voices, tapping the GATE. Both chords held => 6 gates
    // on (sum 6.0). Release only I: its three voices fall to gate-0 while IV's three stay on
    // (sum 3.0) — the held-root tracking proven through the real engine routing.
    let mut plan = Plan::instantiate(chord_rig_tapping(6.0, 1), CFG).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    r.render_block(
        &mut plan,
        &[
            Message::new("/chord/set", Note::new(Pitch::Degree(0), 1.0), 0),
            Message::new("/chord/set", Note::new(Pitch::Degree(3), 1.0), 0),
        ],
        &mut buf,
    );
    approx::assert_relative_eq!(buf[CFG.block_size - 1], 6.0, epsilon = 1e-3); // both chords

    // Release I only: three voices drop, three (IV) remain.
    r.render_block(
        &mut plan,
        &[Message::new(
            "/chord/set",
            Note::new(Pitch::Degree(0), 0.0),
            0,
        )],
        &mut buf,
    );
    approx::assert_relative_eq!(buf[CFG.block_size - 1], 3.0, epsilon = 1e-3); // only IV holds
}

#[test]
fn chord_player_instrument_loads_and_makes_sound() {
    // The shipped instrument: load it, tap the I chord, and render long enough for the
    // slow-attack pad to open — assert audible output through the full saw->filter->env->out
    // chain. (Audible peak, the create-operator gate's honest sound check.)
    let reg = Registry::builtin();
    let graph = load(CHORD_PLAYER, &reg).expect("load chord-player instrument");
    let mut plan = Plan::instantiate(graph, CFG).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; CFG.block_size];

    let press = Message::new("/chord/set", Note::new(Pitch::Degree(0), 1.0), 0);
    let mut peak = 0.0f32;
    // ~2 s at 48k/256 — long enough for the 0.6 s attack to ramp.
    for _ in 0..400 {
        r.render_block(&mut plan, std::slice::from_ref(&press), &mut buf);
        peak = peak.max(buf.iter().fold(0.0, |m, &s| m.max(s.abs())));
    }
    assert!(peak > 0.01, "chord-player produced no sound (peak {peak})");
}
