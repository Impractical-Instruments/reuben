//! The #247 P2 rewrite guard (ADR-0043 §7): every shipped control-block instrument, rewritten
//! to interface pipes + a surface doc, renders **bit-identically** to its pre-rewrite v2
//! original — at rest (a pipe with a declared default is the old literal) and under driven
//! input (the same gesture, sent to the old node address on the original and the pipe's
//! `/<name>/in` address on the rewrite). The pre-rewrite originals are snapshotted verbatim
//! under `tests/fixtures/pre-v3/`; voice/patch refs resolve against the live `instruments/`
//! tree (voices are not rewritten). This is the format_v2.rs shipped-corpus discipline.
//!
//! Pipe naming (pinned here; the rewrite implements to it): the pipe keeps the *public*
//! control name; an internal node colliding with a minted pipe address is renamed
//! (ADR-0017's discipline, applied as a JSON-structural ref sweep):
//! - groovebox: `tempo`, `kick_step1..16` / `snare_step1..16` / `hat_step1..16`,
//!   `kick_vol`/`snare_vol`/`hat_vol` (m2s nodes → `/*_vol_cv`), `tone` (→ `/tone_cv`)
//! - euclidean-drums: `tempo`, per channel (kick/snare/tom/hat): `<ch>_pulses`, `<ch>_steps`,
//!   `<ch>_rotation`, `<ch>_decay`, `<ch>_filter` (m2s → `/<ch>_filter_cv`), `<ch>_level`
//!   (m2s → `/<ch>_level_cv`)
//! - chord-player: `chord` (note pipe; chord node → `/triads`), `key`, `brightness`
//!   (m2s → `/brightness_cv`)
//! - good-button: `notes` (note pipe), `brightness` (m2s → `/brightness_cv`)
//! - strum-harp: `strum` (strum node → `/strummer`), `octaves`, `key`, `brightness`
//!   (m2s → `/brightness_cv`)
//! - djfilter-demo: `tempo`, `filter` (feeds `/filterpos.in`), `resonance`
//! - granulator-demo: `position`, `grain_size`, `pitch`, `density`, `spray` (+ `gain` iff the
//!   granulator operator declares it)

use reuben_core::format::LoadWarning;
use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load_instrument, AudioConfig, Registry};

const BLOCK: usize = 128;
const BLOCKS: usize = 40;

/// Text resources from the live `instruments/` tree; samples synthesized in memory so the
/// granulator renders audibly (a real wav decode belongs to reuben-native, not core tests).
struct InstrumentsDir;
impl ResourceResolver for InstrumentsDir {
    fn resolve(&self, _source: &str) -> Result<SampleBuffer, ResolveError> {
        // A deterministic 100 ms burst: enough frames for grains to land on.
        let samples: Vec<f32> = (0..4800)
            .map(|i| (i as f32 * 0.05).sin() * (1.0 - i as f32 / 4800.0))
            .collect();
        Ok(SampleBuffer::new(vec![samples], 48_000.0))
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

#[derive(Debug, PartialEq)]
struct Rendered {
    channels: Vec<Vec<f32>>,
    outbound: Vec<(usize, Message)>,
    captured: Vec<Vec<f32>>,
}

fn render(
    top: &str,
    blocks: usize,
    messages: impl Fn(usize) -> Vec<Message>,
) -> (Rendered, Vec<LoadWarning>) {
    let loaded = load_instrument(top, &Registry::builtin(), &InstrumentsDir).expect("load");
    let warnings = loaded.warnings;
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
    for b in 0..blocks {
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
    (rendered, warnings)
}

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

fn f32_msg(addr: &str, v: f32, frame: usize) -> Message {
    Message::new(addr, Arg::F32(v), frame)
}

fn note_msg(addr: &str, midi: f32, vel: f32, frame: usize) -> Message {
    Message::new(
        addr,
        Arg::Note(Note::new(Pitch::Absolute(midi), vel)),
        frame,
    )
}

fn degree_msg(addr: &str, degree: i32, vel: f32, frame: usize) -> Message {
    Message::new(
        addr,
        Arg::Note(Note::new(Pitch::Degree(degree), vel)),
        frame,
    )
}

/// One rewritten instrument: its pre-rewrite fixture, and the equivalent gesture stream
/// against the old node addresses (pre) vs the new pipe addresses (post).
struct Rewrite {
    name: &'static str,
    fixture: &'static str,
    /// Blocks to render. 40 suffices for the percussive corpus; chord-player's voice has a
    /// 0.6 s squared attack, so its comparison renders ~0.9 s to clear the vacuousness guard.
    blocks: usize,
    pre: fn(usize) -> Vec<Message>,
    post: fn(usize) -> Vec<Message>,
}

const REWRITES: &[Rewrite] = &[
    Rewrite {
        name: "groovebox",
        fixture: include_str!("fixtures/pre-v3/groovebox.json"),
        blocks: BLOCKS,
        pre: |b| match b {
            5 => vec![f32_msg("/kick/step2", 1.0, 13)],
            12 => vec![f32_msg("/clock/tempo", 140.0, 40)],
            20 => vec![f32_msg("/kick_vol/in", 0.4, 7), f32_msg("/tone/in", 0.9, 7)],
            _ => Vec::new(),
        },
        post: |b| match b {
            5 => vec![f32_msg("/kick_step2/in", 1.0, 13)],
            12 => vec![f32_msg("/tempo/in", 140.0, 40)],
            20 => vec![f32_msg("/kick_vol/in", 0.4, 7), f32_msg("/tone/in", 0.9, 7)],
            _ => Vec::new(),
        },
    },
    Rewrite {
        name: "euclidean-drums",
        fixture: include_str!("fixtures/pre-v3/euclidean-drums.json"),
        blocks: BLOCKS,
        pre: |b| match b {
            4 => vec![f32_msg("/kick_filter/in", -0.5, 21)],
            8 => vec![f32_msg("/clock/tempo", 100.0, 3)],
            14 => vec![
                f32_msg("/kick_eu/pulses", 5.0, 60),
                f32_msg("/snare_env/decay", 0.05, 60),
                f32_msg("/hat_level/in", 0.2, 60),
            ],
            _ => Vec::new(),
        },
        post: |b| match b {
            4 => vec![f32_msg("/kick_filter/in", -0.5, 21)],
            8 => vec![f32_msg("/tempo/in", 100.0, 3)],
            14 => vec![
                f32_msg("/kick_pulses/in", 5.0, 60),
                f32_msg("/snare_decay/in", 0.05, 60),
                f32_msg("/hat_level/in", 0.2, 60),
            ],
            _ => Vec::new(),
        },
    },
    Rewrite {
        name: "chord-player",
        fixture: include_str!("fixtures/pre-v3/chord-player.json"),
        blocks: 340,
        pre: |b| match b {
            1 => vec![degree_msg("/chord/set", 0, 1.0, 30)],
            9 => vec![f32_msg("/brightness/in", 0.85, 10)],
            15 => vec![
                degree_msg("/chord/set", 0, 0.0, 8),
                degree_msg("/chord/set", 4, 1.0, 8),
            ],
            22 => vec![f32_msg("/harmony/root", 55.0, 50)],
            _ => Vec::new(),
        },
        post: |b| match b {
            1 => vec![degree_msg("/chord/in", 0, 1.0, 30)],
            9 => vec![f32_msg("/brightness/in", 0.85, 10)],
            15 => vec![
                degree_msg("/chord/in", 0, 0.0, 8),
                degree_msg("/chord/in", 4, 1.0, 8),
            ],
            22 => vec![f32_msg("/key/in", 55.0, 50)],
            _ => Vec::new(),
        },
    },
    Rewrite {
        name: "good-button",
        fixture: include_str!("fixtures/pre-v3/good-button.json"),
        blocks: BLOCKS,
        pre: |b| match b {
            1 => vec![note_msg("/voicer/notes", 60.0, 1.0, 17)],
            8 => vec![f32_msg("/brightness/in", 0.9, 25)],
            16 => vec![note_msg("/voicer/notes", 60.0, 0.0, 5)],
            _ => Vec::new(),
        },
        post: |b| match b {
            1 => vec![note_msg("/notes/in", 60.0, 1.0, 17)],
            8 => vec![f32_msg("/brightness/in", 0.9, 25)],
            16 => vec![note_msg("/notes/in", 60.0, 0.0, 5)],
            _ => Vec::new(),
        },
    },
    Rewrite {
        name: "strum-harp",
        fixture: include_str!("fixtures/pre-v3/strum-harp.json"),
        blocks: BLOCKS,
        pre: |b| match b {
            2 => vec![
                f32_msg("/strum/position", 0.1, 10),
                f32_msg("/strum/position", 0.5, 60),
                f32_msg("/strum/position", 0.9, 110),
            ],
            10 => vec![
                f32_msg("/harmony/root", 50.0, 4),
                f32_msg("/strum/octaves", 2.0, 4),
            ],
            14 => vec![
                f32_msg("/strum/position", 0.6, 20),
                f32_msg("/brightness/in", 0.7, 20),
            ],
            _ => Vec::new(),
        },
        post: |b| match b {
            2 => vec![
                f32_msg("/strum/in", 0.1, 10),
                f32_msg("/strum/in", 0.5, 60),
                f32_msg("/strum/in", 0.9, 110),
            ],
            10 => vec![f32_msg("/key/in", 50.0, 4), f32_msg("/octaves/in", 2.0, 4)],
            14 => vec![
                f32_msg("/strum/in", 0.6, 20),
                f32_msg("/brightness/in", 0.7, 20),
            ],
            _ => Vec::new(),
        },
    },
    Rewrite {
        name: "djfilter-demo",
        fixture: include_str!("fixtures/pre-v3/djfilter-demo.json"),
        blocks: BLOCKS,
        pre: |b| match b {
            3 => vec![f32_msg("/filterpos/in", -0.7, 33)],
            11 => vec![f32_msg("/djfilter/resonance", 0.8, 90)],
            18 => vec![
                f32_msg("/clock/tempo", 132.0, 12),
                f32_msg("/filterpos/in", 0.6, 12),
            ],
            _ => Vec::new(),
        },
        post: |b| match b {
            3 => vec![f32_msg("/filter/in", -0.7, 33)],
            11 => vec![f32_msg("/resonance/in", 0.8, 90)],
            18 => vec![
                f32_msg("/tempo/in", 132.0, 12),
                f32_msg("/filter/in", 0.6, 12),
            ],
            _ => Vec::new(),
        },
    },
    Rewrite {
        name: "granulator-demo",
        fixture: include_str!("fixtures/pre-v3/granulator-demo.json"),
        blocks: BLOCKS,
        pre: |b| match b {
            3 => vec![f32_msg("/grain/position", 0.25, 44)],
            12 => vec![
                f32_msg("/grain/density", 24.0, 70),
                f32_msg("/grain/spray", 0.1, 70),
            ],
            _ => Vec::new(),
        },
        post: |b| match b {
            3 => vec![f32_msg("/position/in", 0.25, 44)],
            12 => vec![
                f32_msg("/density/in", 24.0, 70),
                f32_msg("/spray/in", 0.1, 70),
            ],
            _ => Vec::new(),
        },
    },
];

fn spec(name: &str) -> &'static Rewrite {
    REWRITES
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| panic!("no rewrite spec for {name}"))
}

fn assert_rewritten(name: &str) {
    let r = spec(name);
    let file = format!("{name}.json");
    let (pre, _) = render(r.fixture, r.blocks, r.pre);
    let (post, warnings) = render(&shipped(&file), r.blocks, r.post);
    assert_nonsilent(&pre, name);
    assert_bit_identical(
        &pre,
        &post,
        &format!("{name}: pipe rewrite vs pre-rewrite v2"),
    );
    // LoadWarning-free after rewrite (the P2 guard): no retired presentation, nothing lost.
    let stale: Vec<_> = warnings
        .iter()
        .map(flat)
        .filter(|w| {
            matches!(
                w,
                LoadWarning::DeprecatedControlBlock { .. }
                    | LoadWarning::DeprecatedPipePresentation { .. }
                    | LoadWarning::Migration { .. }
            )
        })
        .collect();
    assert!(
        stale.is_empty(),
        "{name}: rewritten doc must be clean of retired presentation, got: {stale:?}"
    );
    // Native v3 on disk, not a re-migrated older stamp.
    assert!(
        shipped(&file).contains("\"format_version\": 3"),
        "{name}: rewritten doc is stamped v3"
    );
}

#[test]
fn groovebox_rewrite_renders_bit_identical() {
    assert_rewritten("groovebox");
}

#[test]
fn euclidean_drums_rewrite_renders_bit_identical() {
    assert_rewritten("euclidean-drums");
}

#[test]
fn chord_player_rewrite_renders_bit_identical() {
    assert_rewritten("chord-player");
}

#[test]
fn good_button_rewrite_renders_bit_identical() {
    assert_rewritten("good-button");
}

#[test]
fn strum_harp_rewrite_renders_bit_identical() {
    assert_rewritten("strum-harp");
}

#[test]
fn djfilter_demo_rewrite_renders_bit_identical() {
    assert_rewritten("djfilter-demo");
}

#[test]
fn granulator_demo_rewrite_renders_bit_identical() {
    assert_rewritten("granulator-demo");
}
