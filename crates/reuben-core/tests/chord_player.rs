//! Integration: the V1.3 Chord player end-to-end (ADR-0022) — a `chord` op stacks scale-relative
//! thirds and emits `degree` notes, the engine routes them to a polyphonic Voicer, and the Voicer
//! resolves each degree through the tonal context. Exercises the engine plumbing the operator unit
//! tests can't: routing the chord op's emitted Messages to a real Voicer's voices, and a live chord
//! re-spell on a key change driven through the full graph.

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::vocab::harmony::Harmony;
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load_instrument, AudioConfig, Registry};

const CHORD_PLAYER: &str = include_str!("../../../instruments/chord-player.json");

const CFG: AudioConfig = AudioConfig {
    sample_rate: 48_000.0,
    block_size: 256,
    channels: AudioConfig::MIN_CHANNELS,
    input_channels: 0,
};

fn hz(midi: f32) -> f32 {
    Harmony::default().hz(Pitch::from_midi(midi))
}

/// Test-only probe voices (ADR-0032 session 11): a single `mul_f32_signal` whose `a` operand is the
/// voice's `freq` (or `gate`) interface input — f32_buffer-with-meta, message-settable, ZOH-
/// materialized — and whose `b` defaults to 1.0, so the voice's audio is `a * 1 == a`. Hosting one
/// under the Voicer makes `voicer.audio` equal the summed resolved freq (FREQ) or the summed gate
/// level == count of voices on (GATE), recreating the removed `voicer.freq`/`gate` output taps so the
/// chord-routing assertions stay byte-identical.
const FREQ_PROBE_VOICE: &str = r#"{
  "instrument": "freq-probe-voice",
  "interface": { "inputs": { "freq": "/mul.a" }, "outputs": { "audio": "/mul.out" } },
  "nodes": [ { "type": "mul_f32_signal", "address": "/mul" } ],
  "outputs": [ { "node": "/mul", "port": "out" } ]
}"#;
const GATE_PROBE_VOICE: &str = r#"{
  "instrument": "gate-probe-voice",
  "interface": { "inputs": { "gate": "/mul.a" }, "outputs": { "audio": "/mul.out" } },
  "nodes": [ { "type": "mul_f32_signal", "address": "/mul" } ],
  "outputs": [ { "node": "/mul", "port": "out" } ]
}"#;

/// Serves the test-only probe voices (and nothing else) as instrument-resources.
struct ProbeResolver;
impl ResourceResolver for ProbeResolver {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        match source {
            "freq-probe-voice" => Ok(FREQ_PROBE_VOICE.to_string()),
            "gate-probe-voice" => Ok(GATE_PROBE_VOICE.to_string()),
            // Otherwise read a real voice patch from the repo `instruments/` dir (so the shipped
            // chord-player.json resolves its `chord-player-voice` sub-patch).
            other => {
                let path = format!("{}/../../instruments/{other}", env!("CARGO_MANIFEST_DIR"));
                std::fs::read_to_string(&path)
                    .map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
            }
        }
    }
}

/// A `harmony -> chord -> voicer(poly, <probe>)` host, tapping `voicer.audio`. With the freq probe
/// the tap sums the resolved chord-tone pitches; with the gate probe it sums the gate levels (==
/// number of voices on).
fn chord_rig_tapping(voices: usize, probe: &str) -> Plan {
    let host = format!(
        r#"{{
          "instrument": "chord-rig",
          "nodes": [
            {{ "type": "harmony", "address": "/harmony" }},
            {{ "type": "chord", "address": "/chord" }},
            {{ "type": "voicer", "address": "/voicer", "voice": "v", "config": {{ "voices": {voices} }},
              "inputs": {{ "notes": {{ "from": "/chord" }}, "harmony": {{ "from": "/harmony" }} }} }},
            {{ "type": "output", "address": "/out", "inputs": {{ "audio": {{ "from": "/voicer.audio" }} }} }}
          ],
          "outputs": [ {{ "node": "/out", "port": "audio" }} ],
          "resources": {{ "v": "{probe}" }}
        }}"#
    );
    let graph = load_instrument(&host, &Registry::builtin(), &ProbeResolver)
        .expect("load chord rig")
        .graph;
    Plan::instantiate(graph, CFG).expect("instantiate")
}

/// Tap `freq` (the resolved chord-tone pitches) across all voices.
fn chord_rig(voices: usize) -> Plan {
    chord_rig_tapping(voices, "freq-probe-voice")
}

#[test]
fn tapping_a_triad_sounds_three_chord_tones() {
    // Press chord root degree 0 (the I chord). The chord op emits degrees 0, 2, 4; the Voicer
    // (3 voices) resolves them in C major to C(60), E(64), G(67). The freq-probe voices make
    // voicer.audio the sum of the three voices' resolved freq — assert it equals C+E+G, proving all
    // three chord tones are sounding through the real engine (not just emitted).
    let mut plan = chord_rig(3);
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
    let mut plan = chord_rig_tapping(3, "gate-probe-voice");
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
    let mut plan = chord_rig(3);
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
        &[Message::new("/harmony/root", Arg::F32(62.0), 0)],
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
    let mut plan = chord_rig_tapping(6, "gate-probe-voice");
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
    // The shipped instrument: load it (resolving its hosted voice sub-patch), tap the I chord, and
    // render long enough for the slow-attack pad to open — assert audible output through the full
    // saw->env VCA voice + master filter chain. (Audible peak, the create-operator gate's honest
    // sound check.)
    let graph = load_instrument(CHORD_PLAYER, &Registry::builtin(), &ProbeResolver)
        .expect("load chord-player instrument")
        .graph;
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
