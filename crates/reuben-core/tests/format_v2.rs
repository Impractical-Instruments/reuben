//! Format v2 (ADR-0038): the migration renders **bit-identically**.
//!
//! The direction flip is a pure format change — a v1 document auto-migrated at parse and its
//! hand-written native-v2 equivalent must produce the *same samples*. These tests render both
//! forms of two representative instruments through the real engine and compare exactly:
//!
//! - a **Voicer-hosted** synth (ADR-0032): the voice's `freq`/`gate` interface inputs become
//!   pipes the Voicer drives by message — note-ons/offs at mid-block frames exercise value-pipe
//!   forwarding and signal-pipe materialization timing;
//! - a **nested** effect (ADR-0034): boundary wires, boundary literals, and defaulted control
//!   pipes through the synthesized face.
//!
//! This is the ADR-0026 discipline the shipped-instrument rewrite rides on: the shipped suite
//! (first_sound, groovebox, stereo_pan, …) keeps asserting behavior on the rewritten v2 files,
//! and this file pins v1 ≡ v2 at the sample level.

use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::MemoryResolver;
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load_instrument, AudioConfig, Registry};

const BLOCK: usize = 128;
const BLOCKS: usize = 40;

/// Load `top` (resolving nested refs via `resolver`), instantiate, and render `BLOCKS` blocks,
/// feeding `messages(block)` each block. Returns every master channel concatenated.
fn render(
    top: &str,
    resolver: &MemoryResolver,
    messages: impl Fn(usize) -> Vec<Message>,
) -> Vec<Vec<f32>> {
    let loaded = load_instrument(top, &Registry::builtin(), resolver).expect("load");
    let mut plan =
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, BLOCK)).expect("instantiate");
    let channels = plan.config.channels;
    let mut r = Renderer::new(&plan);
    let mut master: Vec<Vec<f32>> = (0..channels).map(|_| vec![0.0; BLOCK]).collect();
    let mut outbound = Vec::new();
    let mut all: Vec<Vec<f32>> = (0..channels).map(|_| Vec::new()).collect();
    for b in 0..BLOCKS {
        let msgs = messages(b);
        r.render_block_multi(&mut plan, &msgs, &mut master, &mut outbound);
        for (chan, sink) in master.iter().zip(all.iter_mut()) {
            sink.extend_from_slice(chan);
        }
    }
    all
}

fn note(addr: &str, midi: f32, vel: f32, frame: usize) -> Message {
    Message::new(
        addr,
        Arg::Note(Note::new(Pitch::Absolute(midi), vel)),
        frame,
    )
}

/// The note pattern both forms are driven with: on/off/retrigger at mid-block frames, so gate
/// (value-pipe) and freq (signal-pipe) changes land away from block boundaries.
fn voicer_messages(block: usize) -> Vec<Message> {
    match block {
        0 => vec![note("/voicer/notes", 60.0, 1.0, 17)],
        3 => vec![note("/voicer/notes", 64.0, 0.8, 90)],
        10 => vec![note("/voicer/notes", 60.0, 0.0, 5)],
        14 => vec![
            note("/voicer/notes", 64.0, 0.0, 40),
            note("/voicer/notes", 67.0, 1.0, 40),
        ],
        _ => Vec::new(),
    }
}

// The v1 spelling of a subtractive voice + its hosting synth (the pre-ADR-0038 shipped shape:
// target-pointing interface entries, anonymous top-level `outputs`).
const VOICE_V1: &str = r#"{
  "instrument": "voice",
  "interface": {
    "inputs":  { "freq": "/osc.freq", "gate": "/env.gate" },
    "outputs": { "audio": "/out.audio", "active": "/env.active" }
  },
  "nodes": [
    { "type": "oscillator", "address": "/osc" },
    { "type": "filter", "address": "/filter",
      "inputs": { "audio": { "from": "/osc" }, "cutoff": 3000.0 } },
    { "type": "envelope", "address": "/env" },
    { "type": "mul_f32_signal", "address": "/vca",
      "inputs": { "a": { "from": "/filter" }, "b": { "from": "/env.cv" } } },
    { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/vca" } } }
  ],
  "outputs": [ { "node": "/out", "port": "audio" } ]
}"#;

const SYNTH_V1: &str = r#"{
  "instrument": "synth",
  "resources": { "v": "voice.json" },
  "nodes": [
    { "type": "voicer", "address": "/voicer", "voice": "v", "config": { "voices": 3 } },
    { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/voicer.audio" } } }
  ],
  "outputs": [ { "node": "/out", "port": "audio" } ]
}"#;

// The same instrument spelled native v2 (ADR-0038): interface inputs are typed pipes consumed
// by ordinary wire-refs; the master taps are named `interface.outputs` entries.
const VOICE_V2: &str = r#"{
  "format_version": 2,
  "instrument": "voice",
  "interface": {
    "inputs": {
      "freq": { "type": "f32_buffer", "default": 440, "min": 20, "max": 20000,
                "curve": "exp", "unit": "Hz" },
      "gate": { "type": "f32", "default": 0, "min": 0, "max": 1 }
    },
    "outputs": {
      "audio":  { "from": "/out.audio" },
      "active": { "from": "/env.active" }
    }
  },
  "nodes": [
    { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/freq" } } },
    { "type": "filter", "address": "/filter",
      "inputs": { "audio": { "from": "/osc" }, "cutoff": 3000.0 } },
    { "type": "envelope", "address": "/env", "inputs": { "gate": { "from": "/gate" } } },
    { "type": "mul_f32_signal", "address": "/vca",
      "inputs": { "a": { "from": "/filter" }, "b": { "from": "/env.cv" } } },
    { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/vca" } } }
  ]
}"#;

const SYNTH_V2: &str = r#"{
  "format_version": 2,
  "instrument": "synth",
  "resources": { "v": "voice.json" },
  "interface": { "outputs": { "out": { "from": "/out.audio" } } },
  "nodes": [
    { "type": "voicer", "address": "/voicer", "voice": "v", "config": { "voices": 3 } },
    { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/voicer.audio" } } }
  ]
}"#;

#[test]
fn migrated_voicer_synth_renders_bit_identical_to_native_v2() {
    let mut v1 = MemoryResolver::new();
    v1.insert_text("voice.json", VOICE_V1);
    let mut v2 = MemoryResolver::new();
    v2.insert_text("voice.json", VOICE_V2);

    let a = render(SYNTH_V1, &v1, voicer_messages);
    let b = render(SYNTH_V2, &v2, voicer_messages);
    assert_eq!(a.len(), b.len(), "channel count must match");
    for (ch, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert!(x.iter().any(|s| s.abs() > 0.01), "v1 render is silent");
        assert_eq!(
            x, y,
            "channel {ch} drifted between v1-migrated and native v2"
        );
    }
}

// A nested effect (ADR-0034): the child's boundary — an audio input, a swept tone control with
// a child literal, a Value mix control — spelled v1 (targets) and v2 (pipes).
const SPACE_V1: &str = r#"{
  "instrument": "space",
  "interface": {
    "inputs": {
      "in": "/filter.audio",
      "tone": { "target": "/filter.cutoff", "label": "Tone", "min": 200, "max": 8000 },
      "space": "/verb.mix"
    },
    "outputs": { "out": "/verb.audio" }
  },
  "nodes": [
    { "type": "filter", "address": "/filter", "inputs": { "cutoff": 4000.0 } },
    { "type": "reverb", "address": "/verb",
      "inputs": { "audio": { "from": "/filter.audio" }, "mix": 0.35, "room": 0.7 } }
  ]
}"#;

const SPACE_V2: &str = r#"{
  "format_version": 2,
  "instrument": "space",
  "interface": {
    "inputs": {
      "in": { "type": "f32_buffer" },
      "tone": { "type": "f32_buffer", "default": 4000, "min": 200, "max": 8000,
                "curve": "exp", "unit": "Hz", "label": "Tone" },
      "space": { "type": "f32", "default": 0.35, "min": 0, "max": 1 }
    },
    "outputs": { "out": { "from": "/verb.audio" } }
  },
  "nodes": [
    { "type": "filter", "address": "/filter",
      "inputs": { "audio": { "from": "/in" }, "cutoff": { "from": "/tone" } } },
    { "type": "reverb", "address": "/verb",
      "inputs": { "audio": { "from": "/filter.audio" }, "mix": { "from": "/space" }, "room": 0.7 } }
  ]
}"#;

/// A host feeding the nest: an oscillator into `in`, boundary literals on `tone`/`space`.
fn nested_host(version: &str) -> String {
    let (stamp, taps) = if version == "v2" {
        (
            r#""format_version": 2,"#,
            r#""interface": { "outputs": { "out": { "from": "/out.audio" } } },"#,
        )
    } else {
        ("", r#""outputs_v1_marker": null,"#)
    };
    let outputs = if version == "v2" {
        ""
    } else {
        r#", "outputs": [ { "node": "/out", "port": "audio" } ]"#
    };
    format!(
        r#"{{
      {stamp}
      "instrument": "host",
      "resources": {{ "space": "space.json" }},
      {taps}
      "nodes": [
        {{ "type": "oscillator", "address": "/osc" }},
        {{ "type": "subpatch", "address": "/space", "patch": "space",
          "inputs": {{ "in": {{ "from": "/osc.audio" }}, "tone": 2500.0, "space": 0.4 }} }},
        {{ "type": "output", "address": "/out",
          "inputs": {{ "audio": {{ "from": "/space.out" }} }} }}
      ]{outputs}
    }}"#
    )
    .replace(r#""outputs_v1_marker": null,"#, "")
}

#[test]
fn migrated_nested_effect_renders_bit_identical_to_native_v2() {
    let mut v1 = MemoryResolver::new();
    v1.insert_text("space.json", SPACE_V1);
    let mut v2 = MemoryResolver::new();
    v2.insert_text("space.json", SPACE_V2);

    let a = render(&nested_host("v1"), &v1, |_| Vec::new());
    let b = render(&nested_host("v2"), &v2, |_| Vec::new());
    for (ch, (x, y)) in a.iter().zip(b.iter()).enumerate() {
        assert!(x.iter().any(|s| s.abs() > 0.01), "v1 render is silent");
        assert_eq!(
            x, y,
            "channel {ch} drifted between v1-migrated and native v2"
        );
    }
}

#[test]
fn migrated_channel_taps_render_bit_identical_to_native_v2() {
    // ADR-0026's channel-pinned taps: the v1 anonymous `outputs` with `channel` migrate into
    // channel-bound output pipes; both spellings drive the same logical master channels.
    const PAN_V1: &str = r#"{
      "instrument": "pan",
      "nodes": [
        { "type": "oscillator", "address": "/osc" },
        { "type": "pan", "address": "/pan",
          "inputs": { "audio": { "from": "/osc.audio" }, "pan": -0.5 } }
      ],
      "outputs": [
        { "node": "/pan", "port": "left",  "channel": 0 },
        { "node": "/pan", "port": "right", "channel": 1 }
      ]
    }"#;
    const PAN_V2: &str = r#"{
      "format_version": 2,
      "instrument": "pan",
      "interface": {
        "outputs": {
          "main_l": { "from": "/pan.left",  "channel": 0 },
          "main_r": { "from": "/pan.right", "channel": 1 }
        }
      },
      "nodes": [
        { "type": "oscillator", "address": "/osc" },
        { "type": "pan", "address": "/pan",
          "inputs": { "audio": { "from": "/osc.audio" }, "pan": -0.5 } }
      ]
    }"#;
    let none = MemoryResolver::new();
    let a = render(PAN_V1, &none, |_| Vec::new());
    let b = render(PAN_V2, &none, |_| Vec::new());
    assert_eq!(a.len(), 2, "two logical channels");
    assert_eq!(a, b);
    assert_ne!(a[0], a[1], "panned channels differ");
}
