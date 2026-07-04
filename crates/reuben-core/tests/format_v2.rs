//! Format v2 (ADR-0038): the migration renders **bit-identically**.
//!
//! The direction flip is a pure format change — a v1 document auto-migrated at parse and its
//! hand-written native-v2 equivalent must produce the *same output*, on **every** observable
//! surface: master signal channels, the outbound (OSC-out) message vector, and captured Value
//! interface outputs. These tests render both forms through the real engine and compare
//! exactly, across the three host positions (top-level played, subpatch-nested, Voicer-hosted):
//!
//! - a **Voicer-hosted** synth (ADR-0032): the voice's `freq`/`gate` interface inputs become
//!   pipes the Voicer drives by message — note-ons/offs at mid-block frames exercise value-pipe
//!   forwarding and signal-pipe materialization timing;
//! - a **nested** effect (ADR-0034): boundary wires, boundary literals, and defaulted control
//!   pipes through the synthesized face — including an out-of-display-range literal, pinning
//!   that v1's presentational `min`/`max` did not become engine clamps;
//! - **master-tap fidelity** (review #189 F1): duplicate v1 anonymous taps stay duplicated,
//!   channel-pinned taps claim a same-port boundary entry instead of doubling into
//!   pinned + broadcast, and boundary-only outputs stay exact when hosted;
//! - the **message path**: a migrated Value pipe driving an envelope whose `active` feeds both
//!   an `osc_out` (outbound) and a Value interface output (captured);
//! - migration-loss shapes (aliased entries, internally-wired targets, minted-address
//!   collisions, dotted entry names) load with pointed warnings, never fatally, never silently;
//! - a **shipped-document corpus**: the v1 originals of three representative shipped
//!   instruments (embedded verbatim from git history) against the rewritten v2 files on disk.
//!
//! This is the ADR-0026 discipline the shipped-instrument rewrite rides on: the shipped suite
//! (first_sound, groovebox, stereo_pan, …) keeps asserting behavior on the rewritten v2 files,
//! and this file pins v1 ≡ v2 at the sample/message level.

use reuben_core::format::LoadWarning;
use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{MemoryResolver, ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load_instrument, AudioConfig, InstrumentDoc, Registry};

const BLOCK: usize = 128;
const BLOCKS: usize = 40;

/// Everything a render makes observable past the boundary (ADR-0026/0030/0032): the master
/// signal channels, the outbound message vector (block-stamped), and each block's captured
/// Value interface outputs. Bit-identical migration means **all three** match.
#[derive(Debug, PartialEq)]
struct Rendered {
    /// Master channels, concatenated across blocks.
    channels: Vec<Vec<f32>>,
    /// Outbound (OSC-out) messages, tagged with the block they left in.
    outbound: Vec<(usize, Message)>,
    /// Per-block snapshot of the plan's captured Value interface outputs (`Plan::captured`).
    captured: Vec<Vec<f32>>,
}

/// Load `top` (resolving nested refs via `resolver`), instantiate, and render `BLOCKS` blocks,
/// feeding `messages(block)` each block.
fn render(
    top: &str,
    resolver: &dyn ResourceResolver,
    messages: impl Fn(usize) -> Vec<Message>,
) -> Rendered {
    let loaded = load_instrument(top, &Registry::builtin(), resolver).expect("load");
    let mut plan =
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, BLOCK)).expect("instantiate");
    let channels = plan.config.channels;
    let mut r = Renderer::new(&plan);
    let mut master: Vec<Vec<f32>> = (0..channels).map(|_| vec![0.0; BLOCK]).collect();
    let mut rendered = Rendered {
        channels: (0..channels).map(|_| Vec::new()).collect(),
        outbound: Vec::new(),
        captured: Vec::new(),
    };
    let mut outbound = Vec::new();
    for b in 0..BLOCKS {
        let msgs = messages(b);
        outbound.clear();
        r.render_block_multi(&mut plan, &msgs, &[], &mut master, &mut outbound);
        for (chan, sink) in master.iter().zip(rendered.channels.iter_mut()) {
            sink.extend_from_slice(chan);
        }
        rendered
            .outbound
            .extend(outbound.iter().map(|m| (b, m.clone())));
        rendered.captured.push(plan.captured.clone());
    }
    rendered
}

/// Assert every observable surface matches: channels, outbound messages, captured Values.
fn assert_bit_identical(a: &Rendered, b: &Rendered, what: &str) {
    assert_eq!(
        a.channels.len(),
        b.channels.len(),
        "{what}: channel count must match"
    );
    for (ch, (x, y)) in a.channels.iter().zip(b.channels.iter()).enumerate() {
        assert_eq!(x, y, "{what}: channel {ch} drifted");
    }
    assert_eq!(a.outbound, b.outbound, "{what}: outbound messages drifted");
    assert_eq!(
        a.captured, b.captured,
        "{what}: captured Value interface outputs drifted"
    );
}

fn assert_nonsilent(r: &Rendered, what: &str) {
    assert!(
        r.channels
            .iter()
            .any(|ch| ch.iter().any(|s| s.abs() > 0.01)),
        "{what}: render is silent — the comparison would be vacuous"
    );
}

/// Peel `Nested` provenance wrappers off a warning.
fn flat(w: &LoadWarning) -> &LoadWarning {
    match w {
        LoadWarning::Nested { warning, .. } => flat(warning),
        other => other,
    }
}

fn note(addr: &str, midi: f32, vel: f32, frame: usize) -> Message {
    Message::new(
        addr,
        Arg::Note(Note::new(Pitch::Absolute(midi), vel)),
        frame,
    )
}

/// The note pattern the Voicer fixtures are driven with: on/off/retrigger at mid-block frames,
/// so gate (value-pipe) and freq (signal-pipe) changes land away from block boundaries.
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
    assert_nonsilent(&a, "voicer synth");
    assert_bit_identical(&a, &b, "voicer synth (v1-migrated vs native v2)");
}

#[test]
fn migration_produces_exactly_the_native_v2_document() {
    // Stronger than render equality: the migrated *document* IS the native v2 spelling —
    // derived types/ranges/defaults, flipped consumer wires, claimed taps, all of it.
    let reg = Registry::builtin();
    for (v1, v2, what) in [
        (VOICE_V1, VOICE_V2, "voice"),
        (SPACE_V1, SPACE_V2, "space"),
        (SENDER_V1, SENDER_V2, "sender"),
    ] {
        let migrated = InstrumentDoc::from_json(v1, &reg).expect("migrate v1");
        let native = InstrumentDoc::from_json(v2, &reg).expect("parse v2");
        assert_eq!(migrated, native, "{what}: migrated doc != native v2 doc");
    }
}

// A nested effect (ADR-0034): the child's boundary — an audio input, a swept tone control with
// a child literal and v1 *presentational* min/max, a Value mix control — spelled v1 (targets)
// and v2 (pipes). The v1 `tone` narrowing (200..8000) was display-only: the engine enforced the
// inner cutoff's 20..20000 — so the native-v2 equivalent declares the inner range (the migrated
// pipe's engine-enforced range must not clamp harder than v1 did — review #189 F5).
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
      "tone": { "type": "f32_buffer", "default": 4000, "min": 20, "max": 20000,
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
fn nested_host(version: &str, tone: f64) -> String {
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
          "inputs": {{ "in": {{ "from": "/osc.audio" }}, "tone": {tone}, "space": 0.4 }} }},
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

    let a = render(&nested_host("v1", 2500.0), &v1, |_| Vec::new());
    let b = render(&nested_host("v2", 2500.0), &v2, |_| Vec::new());
    assert_nonsilent(&a, "nested effect");
    assert_bit_identical(&a, &b, "nested effect (v1-migrated vs native v2)");
}

#[test]
fn out_of_display_range_control_is_not_clamped_harder_than_v1() {
    // Review #189 F5: v1's `tone` min/max (200..8000) were presentational — a literal 15000
    // reached the inner cutoff, clamped only by the cutoff's own 20..20000. The migrated pipe
    // must behave the same (its engine range is the inner port's), NOT clamp at 8000.
    let mut v1 = MemoryResolver::new();
    v1.insert_text("space.json", SPACE_V1);
    let mut v2 = MemoryResolver::new();
    v2.insert_text("space.json", SPACE_V2);

    let a = render(&nested_host("v1", 15000.0), &v1, |_| Vec::new());
    let b = render(&nested_host("v2", 15000.0), &v2, |_| Vec::new());
    assert_nonsilent(&a, "out-of-display-range tone");
    assert_bit_identical(&a, &b, "tone=15000 (v1-migrated vs native v2, inner range)");

    // Prove the assertion has teeth: a pipe that DID enforce the old display narrowing
    // (min 200 / max 8000, clamping the literal to 8000) renders audibly differently.
    const SPACE_V2_NARROWED: &str = r#"{
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
    let mut narrowed = MemoryResolver::new();
    narrowed.insert_text("space.json", SPACE_V2_NARROWED);
    let c = render(&nested_host("v2", 15000.0), &narrowed, |_| Vec::new());
    assert_ne!(
        a.channels, c.channels,
        "an engine-enforced 8000 Hz clamp must be audible — otherwise this test proves nothing"
    );
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
    assert_eq!(a.channels.len(), 2, "two logical channels");
    assert_bit_identical(&a, &b, "channel-pinned taps");
    assert_ne!(a.channels[0], a.channels[1], "panned channels differ");
}

// ---------------------------------------------------------------------------------------------
// Master-tap fidelity (review #189 F1): the migrated tap multiset is exactly v1's.
// ---------------------------------------------------------------------------------------------

#[test]
fn duplicate_v1_taps_stay_duplicated() {
    // v1 summed two identical anonymous taps (2x amplitude). Migration keeps one entry PER
    // tap — collapsing them to one would halve the render.
    const DUP_V1: &str = r#"{
      "instrument": "dup",
      "nodes": [ { "type": "oscillator", "address": "/osc" } ],
      "outputs": [
        { "node": "/osc", "port": "audio" },
        { "node": "/osc", "port": "audio" }
      ]
    }"#;
    const DUP_V2: &str = r#"{
      "format_version": 2,
      "instrument": "dup",
      "interface": {
        "outputs": {
          "out":   { "from": "/osc.audio" },
          "out_2": { "from": "/osc.audio" }
        }
      },
      "nodes": [ { "type": "oscillator", "address": "/osc" } ]
    }"#;
    const SINGLE_V2: &str = r#"{
      "format_version": 2,
      "instrument": "dup",
      "interface": { "outputs": { "out": { "from": "/osc.audio" } } },
      "nodes": [ { "type": "oscillator", "address": "/osc" } ]
    }"#;
    let reg = Registry::builtin();
    let migrated = InstrumentDoc::from_json(DUP_V1, &reg).expect("migrate");
    let iface = migrated.interface.as_ref().expect("interface");
    assert_eq!(
        iface.outputs.keys().collect::<Vec<_>>(),
        ["out", "out_2"],
        "one generated entry per v1 tap"
    );

    let none = MemoryResolver::new();
    let a = render(DUP_V1, &none, |_| Vec::new());
    let b = render(DUP_V2, &none, |_| Vec::new());
    let single = render(SINGLE_V2, &none, |_| Vec::new());
    assert_nonsilent(&a, "duplicate taps");
    assert_bit_identical(&a, &b, "duplicate v1 taps (migrated vs native two-entry)");
    assert_ne!(
        a.channels[0], single.channels[0],
        "two taps must sum louder than one — otherwise the multiplicity assertion is vacuous"
    );
}

#[test]
fn pinned_taps_claim_a_same_port_boundary_entry_instead_of_doubling() {
    // Review #189 F1's worst case: v1 pinned taps on a port an interface entry also feeds.
    // The v1 render was ONLY the pinned taps (interface entries never tapped in v1); the old
    // exact-(port,channel) dedup missed (None != Some(0)) and produced pinned + broadcast —
    // doubled amplitude with bleed onto both channels. Claiming fixes it: the entry becomes
    // the ch-0 tap, the second tap generates its own entry.
    const PINNED_V1: &str = r#"{
      "instrument": "pinned",
      "interface": { "outputs": { "audio": "/osc.audio" } },
      "nodes": [ { "type": "oscillator", "address": "/osc" } ],
      "outputs": [
        { "node": "/osc", "port": "audio", "channel": 0 },
        { "node": "/osc", "port": "audio", "channel": 1 }
      ]
    }"#;
    const PINNED_V2: &str = r#"{
      "format_version": 2,
      "instrument": "pinned",
      "interface": {
        "outputs": {
          "audio": { "from": "/osc.audio", "channel": 0 },
          "out":   { "from": "/osc.audio", "channel": 1 }
        }
      },
      "nodes": [ { "type": "oscillator", "address": "/osc" } ]
    }"#;
    let reg = Registry::builtin();
    let migrated = InstrumentDoc::from_json(PINNED_V1, &reg).expect("migrate");
    let native = InstrumentDoc::from_json(PINNED_V2, &reg).expect("parse");
    assert_eq!(migrated, native, "the entry claims the ch-0 tap");

    let none = MemoryResolver::new();
    let a = render(PINNED_V1, &none, |_| Vec::new());
    let b = render(PINNED_V2, &none, |_| Vec::new());
    assert_nonsilent(&a, "pinned taps");
    assert_bit_identical(&a, &b, "pinned taps + same-port entry");
    // The boundary name survives the claim: a Voicer/host still finds `audio`.
    let loaded = load_instrument(PINNED_V1, &reg, &none).expect("load");
    assert!(loaded.graph.interface.outputs.contains_key("audio"));
    // Exactly the two pinned taps — no third broadcast tap.
    assert_eq!(
        loaded.graph.outputs.len(),
        2,
        "taps: {:?}",
        loaded.graph.outputs
    );
    assert!(
        loaded.graph.outputs.iter().all(|(_, _, ch)| ch.is_some()),
        "no phantom broadcast tap"
    );
}

// A child whose v1 boundary declares a signal output that is NOT anonymously tapped ("wet"),
// next to one that is ("out"): the classic send-effect shape.
const WETDRY_V1: &str = r#"{
  "instrument": "wetdry",
  "interface": {
    "inputs":  { "in": "/dry.audio" },
    "outputs": { "out": "/dry.audio", "wet": "/verb.audio" }
  },
  "nodes": [
    { "type": "filter", "address": "/dry", "inputs": { "cutoff": 4000.0 } },
    { "type": "reverb", "address": "/verb",
      "inputs": { "audio": { "from": "/dry.audio" }, "mix": 1.0, "room": 0.8 } }
  ],
  "outputs": [ { "node": "/dry", "port": "audio" } ]
}"#;

const WETDRY_V2: &str = r#"{
  "format_version": 2,
  "instrument": "wetdry",
  "interface": {
    "inputs":  { "in": { "type": "f32_buffer" } },
    "outputs": { "out": { "from": "/dry.audio" }, "wet": { "from": "/verb.audio" } }
  },
  "nodes": [
    { "type": "filter", "address": "/dry",
      "inputs": { "audio": { "from": "/in" }, "cutoff": 4000.0 } },
    { "type": "reverb", "address": "/verb",
      "inputs": { "audio": { "from": "/dry.audio" }, "mix": 1.0, "room": 0.8 } }
  ]
}"#;

#[test]
fn boundary_only_v1_outputs_are_exact_when_hosted() {
    // The position boundary-only outputs were USED in: a host wiring the child's `wet`/`out`.
    // Child master taps never cross the boundary (ADR-0034 §4), so hosted behavior is exact.
    fn host(version: &str) -> String {
        let (stamp, taps, outputs) = if version == "v2" {
            (
                r#""format_version": 2,"#,
                r#""interface": { "outputs": { "out": { "from": "/mix.out" } } },"#,
                "",
            )
        } else {
            (
                "",
                "",
                r#", "outputs": [ { "node": "/mix", "port": "out" } ]"#,
            )
        };
        format!(
            r#"{{
          {stamp}
          "instrument": "host",
          "resources": {{ "fx": "wetdry.json" }},
          {taps}
          "nodes": [
            {{ "type": "oscillator", "address": "/osc" }},
            {{ "type": "subpatch", "address": "/fx", "patch": "fx",
              "inputs": {{ "in": {{ "from": "/osc.audio" }} }} }},
            {{ "type": "add_f32_signal", "address": "/mix",
              "inputs": {{ "a": {{ "from": "/fx.out" }}, "b": {{ "from": "/fx.wet" }} }} }}
          ]{outputs}
        }}"#
        )
    }
    let mut v1 = MemoryResolver::new();
    v1.insert_text("wetdry.json", WETDRY_V1);
    let mut v2 = MemoryResolver::new();
    v2.insert_text("wetdry.json", WETDRY_V2);
    let a = render(&host("v1"), &v1, |_| Vec::new());
    let b = render(&host("v2"), &v2, |_| Vec::new());
    assert_nonsilent(&a, "hosted wet/dry");
    assert_bit_identical(&a, &b, "boundary-only outputs, hosted");
}

#[test]
fn boundary_only_v1_output_played_top_level_is_the_documented_adr_fork() {
    // ADR-0038 §4 unified "boundary output" with "master tap": a channel-less signal output
    // pipe at top level IS a broadcast tap. v1 kept the two separate — `wet` above produced no
    // sound at top level (only the anonymous `outputs` tapped). The consolidated block has no
    // way to spell "signal boundary output, not a tap", so this ONE v1 shape diverges when the
    // migrated document is played top-level: `wet` now broadcasts alongside the dry tap. This
    // test documents that fork (reported on PR #189) rather than hiding it; everything
    // hosted/nested — the position these entries were authored for — is exact (test above).
    let reg = Registry::builtin();
    let none = MemoryResolver::new();
    let loaded = load_instrument(WETDRY_V1, &reg, &none).expect("load");
    let taps = &loaded.graph.outputs;
    assert_eq!(
        taps.len(),
        2,
        "migrated top-level taps: v1's dry tap + the fork's wet broadcast ({taps:?})"
    );
    assert_eq!(
        taps.iter().filter(|(_, _, ch)| ch.is_none()).count(),
        2,
        "both broadcast: the claimed dry entry and the boundary-only wet entry"
    );
}

// ---------------------------------------------------------------------------------------------
// Migration-loss shapes (review #189 F2/F4/F6): loud, degraded, never fatal, never silent.
// ---------------------------------------------------------------------------------------------

#[test]
fn aliased_v1_entries_keep_the_first_and_warn_on_the_rest() {
    // Two v1 names for one target: the flip can wire the port from only one pipe. The first
    // (BTreeMap name order — deterministic, not declaration-order-dependent) wins; the second
    // drops with a warning naming both, and goes dark instead of vanishing.
    const ALIASED: &str = r#"{
      "instrument": "aliased",
      "interface": { "inputs": { "cut": "/f.cutoff", "tone": "/f.cutoff" } },
      "nodes": [ { "type": "filter", "address": "/f" } ]
    }"#;
    let reg = Registry::builtin();
    let none = MemoryResolver::new();
    let loaded = load_instrument(ALIASED, &reg, &none).expect("aliases never turn fatal");
    assert!(
        loaded.graph.interface.inputs.contains_key("cut"),
        "first alias (name order) migrates"
    );
    assert!(
        !loaded.graph.interface.inputs.contains_key("tone"),
        "second alias cannot also wire the port"
    );
    assert!(
        loaded.warnings.iter().any(|w| matches!(
            flat(w),
            LoadWarning::Migration { name, detail }
                if name == "tone" && detail.contains("aliases") && detail.contains("cut")
        )),
        "the dropped alias is warned, pointing at the survivor: {:?}",
        loaded.warnings
    );

    // Hosted, a wire onto the dropped name degrades dark (dropped + build keeps loading),
    // exactly like a reference to an unavailable nested target — never UnknownInput-fatal.
    const HOST: &str = r#"{"instrument":"h","resources":{"c":"c.json"},"nodes":[
        {"type":"oscillator","address":"/lfo"},
        {"type":"subpatch","address":"/sub","patch":"c",
         "inputs":{"tone":{"from":"/lfo.cv"}}}]}"#;
    let mut resolver = MemoryResolver::new();
    resolver.insert_text("c.json", ALIASED);
    load_instrument(HOST, &reg, &resolver).expect("a wire onto the dark alias degrades");
}

#[test]
fn internally_wired_value_target_drops_loudly_and_the_voice_keeps_loading() {
    // Review #189 F2: v1 legally merged a host-driven boundary name with an internal wire on
    // Value/Event inputs (`/env.gate` below takes /env2.active internally AND was exposed as
    // `gate`). The flip cannot express the merge — the entry drops, but with a Migration
    // warning naming it (the Voicer's gate goes dead, which the author must hear about), and
    // the hosting instrument still loads.
    const MERGED_VOICE: &str = r#"{
      "instrument": "voice",
      "interface": {
        "inputs":  { "freq": "/osc.freq", "gate": "/env.gate" },
        "outputs": { "audio": "/out.audio", "active": "/env.active" }
      },
      "nodes": [
        { "type": "oscillator", "address": "/osc" },
        { "type": "envelope", "address": "/env",
          "inputs": { "gate": { "from": "/env2.active" } } },
        { "type": "envelope", "address": "/env2" },
        { "type": "mul_f32_signal", "address": "/vca",
          "inputs": { "a": { "from": "/osc.audio" }, "b": { "from": "/env.cv" } } },
        { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/vca" } } }
      ],
      "outputs": [ { "node": "/out", "port": "audio" } ]
    }"#;
    let reg = Registry::builtin();
    let mut resolver = MemoryResolver::new();
    resolver.insert_text("voice.json", MERGED_VOICE);
    let loaded = load_instrument(SYNTH_V1, &reg, &resolver).expect("the host keeps loading");
    assert!(
        loaded.warnings.iter().any(|w| matches!(
            flat(w),
            LoadWarning::Migration { name, detail }
                if name == "gate" && detail.contains("internal wire")
        )),
        "the dropped merge-legal entry is warned through the Voicer host: {:?}",
        loaded.warnings
    );
}

#[test]
fn a_node_at_a_minted_address_steps_aside_with_its_references() {
    // Review #189 F4: entry "filter" mints /filter — an address the document's own /filter
    // node holds. Legal v1 (entries minted nothing); fatal DuplicateAddress would break
    // ADR-0036 §4. The node renames aside and every reference follows.
    const COLLIDING: &str = r#"{
      "instrument": "colliding",
      "interface": { "inputs": { "filter": "/filter.cutoff" } },
      "nodes": [
        { "type": "oscillator", "address": "/osc" },
        { "type": "filter", "address": "/filter",
          "inputs": { "audio": { "from": "/osc.audio" } } },
        { "type": "output", "address": "/out",
          "inputs": { "audio": { "from": "/filter.audio" } } }
      ],
      "outputs": [ { "node": "/out", "port": "audio" } ]
    }"#;
    let reg = Registry::builtin();
    let none = MemoryResolver::new();
    let loaded = load_instrument(COLLIDING, &reg, &none).expect("collision never turns fatal");
    let g = &loaded.graph;
    let pipe = g.find("/filter").expect("the minted pipe owns /filter");
    assert_eq!(g.nodes[pipe].descriptor.type_name, "pipe");
    let renamed = g.find("/filter_2").expect("the node stepped aside");
    assert_eq!(g.nodes[renamed].descriptor.type_name, "filter");
    // The pipe feeds the renamed node's cutoff; the output node follows the rename.
    assert!(g
        .connections
        .iter()
        .any(|c| c.src == pipe && c.dst == renamed));
    assert!(loaded.warnings.iter().any(|w| matches!(
        flat(w),
        LoadWarning::Migration { name, detail }
            if name == "/filter" && detail.contains("/filter_2")
    )));
    // And it plays: the render path survived the rewrite.
    let r = render(COLLIDING, &none, |_| Vec::new());
    assert_nonsilent(&r, "renamed-node instrument");

    // The same collision in a NATIVE v2 document stays the fatal DuplicateAddress: minting is
    // declared there, not inherited from v1.
    const NATIVE: &str = r#"{
      "format_version": 2,
      "instrument": "native",
      "interface": { "inputs": { "filter": { "type": "f32" } } },
      "nodes": [ { "type": "filter", "address": "/filter" } ]
    }"#;
    assert!(matches!(
        load_instrument(NATIVE, &reg, &none),
        Err(reuben_core::format::LoadError::DuplicateAddress(a)) if a == "/filter"
    ));
}

#[test]
fn dotted_v1_entry_names_mint_and_resolve() {
    // Review #189 F4: a dotted v1 entry name ("my.tone" — just a map key in v1) mints
    // /my.tone; the flipped consumer ref must resolve to the pipe, not misparse as node "/my"
    // port "tone".
    const DOTTED: &str = r#"{
      "instrument": "dotted",
      "interface": { "inputs": { "my.tone": "/f.cutoff" } },
      "nodes": [ { "type": "filter", "address": "/f" } ]
    }"#;
    let reg = Registry::builtin();
    let none = MemoryResolver::new();
    let loaded = load_instrument(DOTTED, &reg, &none).expect("dotted names keep loading");
    let g = &loaded.graph;
    let pipe = g.find("/my.tone").expect("pipe minted verbatim");
    assert_eq!(g.nodes[pipe].descriptor.type_name, "pipe");
    let f = g.find("/f").unwrap();
    assert!(
        g.connections.iter().any(|c| c.src == pipe && c.dst == f),
        "the consumer wire resolves to the pipe"
    );
    assert!(g.interface.inputs.contains_key("my.tone"));
}

#[test]
fn arg_target_entries_drop_loudly_instead_of_refusing_the_document() {
    // Review #189 F6: v1 accepted interface entries targeting ANY input port by inheritance —
    // including `osc_out.in` (the type-agnostic Arg pass-through). The pipe model has no Arg
    // form, so the entry drops with a warning; the document keeps loading (ADR-0036 §4), and
    // the rest of its boundary is intact.
    const ARG_TARGET: &str = r#"{
      "instrument": "argy",
      "interface": { "inputs": { "send": "/tap.in", "cutoff": "/f.cutoff" } },
      "nodes": [
        { "type": "filter", "address": "/f" },
        { "type": "osc_out", "address": "/tap" }
      ]
    }"#;
    let reg = Registry::builtin();
    let none = MemoryResolver::new();
    let loaded = load_instrument(ARG_TARGET, &reg, &none).expect("Arg targets never turn fatal");
    assert!(
        loaded.graph.interface.inputs.contains_key("cutoff"),
        "the migratable entry still migrates"
    );
    assert!(!loaded.graph.interface.inputs.contains_key("send"));
    assert!(
        loaded.warnings.iter().any(|w| matches!(
            flat(w),
            LoadWarning::Migration { name, detail }
                if name == "send" && detail.contains("no pipe form")
        )),
        "warnings: {:?}",
        loaded.warnings
    );
}

// ---------------------------------------------------------------------------------------------
// The message path (review #189 F7): outbound messages and captured Value outputs, asserted.
// ---------------------------------------------------------------------------------------------

// A migrated Value pipe (`gate`) drives an envelope whose `active` feeds BOTH an `osc_out`
// (observable on the outbound vector) and a Value interface output (observable in
// `Plan::captured`) — the full message path through a migrated pipe.
const SENDER_V1: &str = r#"{
  "instrument": "sender",
  "interface": {
    "inputs":  { "gate": "/env.gate" },
    "outputs": { "active": "/env.active" }
  },
  "nodes": [
    { "type": "envelope", "address": "/env" },
    { "type": "osc_out", "address": "/send", "inputs": { "in": { "from": "/env.active" } } }
  ]
}"#;

const SENDER_V2: &str = r#"{
  "format_version": 2,
  "instrument": "sender",
  "interface": {
    "inputs":  { "gate": { "type": "f32", "default": 0, "min": 0, "max": 1 } },
    "outputs": { "active": { "from": "/env.active" } }
  },
  "nodes": [
    { "type": "envelope", "address": "/env", "inputs": { "gate": { "from": "/gate" } } },
    { "type": "osc_out", "address": "/send", "inputs": { "in": { "from": "/env.active" } } }
  ]
}"#;

#[test]
fn message_path_through_a_migrated_value_pipe_is_bit_identical() {
    // Both forms expose the same pipe surface (/gate/in) — migration mints exactly the native
    // address — so one drive pattern exercises both.
    let gate = |block: usize| -> Vec<Message> {
        match block {
            1 => vec![Message::new("/gate/in", Arg::F32(1.0), 10)],
            8 => vec![Message::new("/gate/in", Arg::F32(0.0), 60)],
            _ => Vec::new(),
        }
    };
    let none = MemoryResolver::new();
    let a = render(SENDER_V1, &none, gate);
    let b = render(SENDER_V2, &none, gate);
    assert!(
        !a.outbound.is_empty(),
        "the envelope's active transitions must reach the outbound sink"
    );
    assert!(
        a.captured.iter().any(|c| c.contains(&1.0)) && a.captured.iter().any(|c| c.contains(&0.0)),
        "the captured `active` Value output must see both states: {:?}",
        a.captured
    );
    assert_bit_identical(&a, &b, "message path (v1-migrated vs native v2)");
}

// ---------------------------------------------------------------------------------------------
// Shipped-document corpus (review #189 F7): v1 originals from git history vs the rewritten v2
// files on disk. Coverage: three representative families — a Voicer rig with a master effect
// and an anonymous broadcast tap (reverb), a channel-pinned stereo rig (stereo-autopan), and
// the nesting + presentational-range chain (nested-space → patches/space.json). Sample-backed
// documents (sampler*, granulator, …) are excluded: their fidelity rides on the same machinery
// (voicer + pipes + taps) but needs decoded audio fixtures; the loader paths they add
// (ResourceStore binding) are format-version-independent.
// ---------------------------------------------------------------------------------------------

/// Resolves nested refs from the repo `instruments/` dir (the v2 side reads what ships).
struct InstrumentsDir;
impl ResourceResolver for InstrumentsDir {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/../../instruments/{source}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }
}

fn shipped(name: &str) -> String {
    InstrumentsDir
        .resolve_text(name)
        .unwrap_or_else(|e| panic!("read shipped {name}: {e}"))
}

// The v1 originals, verbatim from git history (pre-rewrite tip 9d03457, `git show
// 9d03457:instruments/<path>`), doc strings elided — they are not rendered.
const V1_REVERB: &str = r#"{
  "instrument": "reverb",
  "nodes": [
    { "address": "/voicer", "config": { "voices": 8 }, "type": "voicer", "voice": "reverb-voice" },
    { "address": "/reverb",
      "inputs": { "audio": { "from": "/voicer.audio" }, "damp": 0.5, "mix": 0.3, "room": 0.7 },
      "type": "reverb" },
    { "address": "/out", "inputs": { "audio": { "from": "/reverb" } }, "type": "output" }
  ],
  "outputs": [ { "node": "/out", "port": "audio" } ],
  "resources": { "reverb-voice": "voices/reverb-voice.json" }
}"#;

const V1_REVERB_VOICE: &str = r#"{
  "instrument": "reverb-voice",
  "interface": {
    "inputs": { "freq": "/osc.freq", "gate": "/env.gate" },
    "outputs": { "audio": "/out.audio", "active": "/env.active" }
  },
  "nodes": [
    { "address": "/osc", "type": "oscillator" },
    { "address": "/filter", "inputs": { "audio": { "from": "/osc" }, "cutoff": 3000.0 }, "type": "filter" },
    { "address": "/env", "type": "envelope" },
    { "address": "/env_curve", "inputs": { "x": { "from": "/env.cv" } }, "type": "power_f32_signal" },
    { "address": "/env_vca", "inputs": { "a": { "from": "/filter" }, "b": { "from": "/env_curve" } }, "type": "mul_f32_signal" },
    { "address": "/out", "inputs": { "audio": { "from": "/env_vca" } }, "type": "output" }
  ],
  "outputs": [ { "node": "/out", "port": "audio" } ]
}"#;

const V1_STEREO_AUTOPAN: &str = r#"{
  "instrument": "stereo-autopan",
  "nodes": [
    { "address": "/voicer", "config": { "voices": 8 }, "type": "voicer", "voice": "autopan-voice" },
    { "address": "/autopan", "inputs": { "center": 0.0, "depth": 1.0, "rate": 0.5 }, "type": "lfo" },
    { "address": "/pan",
      "inputs": { "audio": { "from": "/voicer.audio" }, "pan": { "from": "/autopan" } }, "type": "pan" }
  ],
  "outputs": [
    { "channel": 0, "node": "/pan", "port": "left" },
    { "channel": 1, "node": "/pan", "port": "right" }
  ],
  "resources": { "autopan-voice": "voices/autopan-voice.json" }
}"#;

const V1_AUTOPAN_VOICE: &str = r#"{
  "instrument": "autopan-voice",
  "interface": {
    "inputs": { "freq": "/osc.freq", "gate": "/env.gate" },
    "outputs": { "audio": "/out.audio", "active": "/env.active" }
  },
  "nodes": [
    { "address": "/osc", "inputs": { "waveform": "Saw" }, "type": "oscillator" },
    { "address": "/filter", "inputs": { "audio": { "from": "/osc" }, "cutoff": 2500.0 }, "type": "filter" },
    { "address": "/env", "type": "envelope" },
    { "address": "/env_curve", "inputs": { "x": { "from": "/env.cv" } }, "type": "power_f32_signal" },
    { "address": "/env_vca", "inputs": { "a": { "from": "/filter" }, "b": { "from": "/env_curve" } }, "type": "mul_f32_signal" },
    { "address": "/out", "inputs": { "audio": { "from": "/env_vca" } }, "type": "output" }
  ],
  "outputs": [ { "node": "/out", "port": "audio" } ]
}"#;

const V1_NESTED_SPACE: &str = r#"{
  "instrument": "nested-space",
  "resources": {
    "default-voice": "voices/default-voice.json",
    "space": "patches/space.json"
  },
  "nodes": [
    { "address": "/voicer", "config": { "voices": 8 }, "type": "voicer", "voice": "default-voice" },
    { "address": "/space",
      "inputs": { "in": { "from": "/voicer.audio" }, "space": 0.4, "tone": 2500.0 },
      "patch": "space", "type": "subpatch" },
    { "address": "/out", "inputs": { "audio": { "from": "/space.out" } }, "type": "output" }
  ],
  "outputs": [ { "node": "/out", "port": "audio" } ]
}"#;

const V1_SPACE_PATCH: &str = r#"{
  "instrument": "space",
  "interface": {
    "inputs": {
      "in": "/filter.audio",
      "tone": { "target": "/filter.cutoff", "label": "Tone", "widget": "knob", "min": 200, "max": 8000 },
      "space": { "target": "/verb.mix", "label": "Space" }
    },
    "outputs": { "out": "/verb.audio" }
  },
  "nodes": [
    { "address": "/filter", "inputs": { "cutoff": 4000.0 }, "type": "filter" },
    { "address": "/verb",
      "inputs": { "audio": { "from": "/filter.audio" }, "mix": 0.35, "room": 0.7 }, "type": "reverb" }
  ]
}"#;

const V1_DEFAULT_VOICE: &str = r#"{
  "instrument": "default-voice",
  "interface": {
    "inputs": { "freq": "/osc.freq", "gate": "/env.gate" },
    "outputs": { "audio": "/out.audio", "active": "/env.active" }
  },
  "nodes": [
    { "address": "/osc", "type": "oscillator" },
    { "address": "/filter", "inputs": { "audio": { "from": "/osc" }, "cutoff": 3000.0 }, "type": "filter" },
    { "address": "/env", "type": "envelope" },
    { "address": "/env_curve", "inputs": { "x": { "from": "/env.cv" } }, "type": "power_f32_signal" },
    { "address": "/env_vca", "inputs": { "a": { "from": "/filter" }, "b": { "from": "/env_curve" } }, "type": "mul_f32_signal" },
    { "address": "/out", "inputs": { "audio": { "from": "/env_vca" } }, "type": "output" }
  ],
  "outputs": [ { "node": "/out", "port": "audio" } ]
}"#;

#[test]
fn shipped_reverb_renders_bit_identical_to_its_v1_original() {
    let mut v1 = MemoryResolver::new();
    v1.insert_text("voices/reverb-voice.json", V1_REVERB_VOICE);
    let a = render(V1_REVERB, &v1, voicer_messages);
    let b = render(&shipped("reverb.json"), &InstrumentsDir, voicer_messages);
    assert_nonsilent(&a, "reverb");
    assert_bit_identical(&a, &b, "shipped reverb.json vs its v1 original");
}

#[test]
fn shipped_stereo_autopan_renders_bit_identical_to_its_v1_original() {
    let mut v1 = MemoryResolver::new();
    v1.insert_text("voices/autopan-voice.json", V1_AUTOPAN_VOICE);
    let a = render(V1_STEREO_AUTOPAN, &v1, voicer_messages);
    let b = render(
        &shipped("stereo-autopan.json"),
        &InstrumentsDir,
        voicer_messages,
    );
    assert_nonsilent(&a, "stereo-autopan");
    assert_eq!(a.channels.len(), 2);
    assert_ne!(a.channels[0], a.channels[1], "the pan actually pans");
    assert_bit_identical(&a, &b, "shipped stereo-autopan.json vs its v1 original");
}

#[test]
fn shipped_nested_space_renders_bit_identical_to_its_v1_original() {
    let mut v1 = MemoryResolver::new();
    v1.insert_text("voices/default-voice.json", V1_DEFAULT_VOICE);
    v1.insert_text("patches/space.json", V1_SPACE_PATCH);
    let a = render(V1_NESTED_SPACE, &v1, voicer_messages);
    let b = render(
        &shipped("nested-space.json"),
        &InstrumentsDir,
        voicer_messages,
    );
    assert_nonsilent(&a, "nested-space");
    assert_bit_identical(&a, &b, "shipped nested-space.json vs its v1 original");
}
