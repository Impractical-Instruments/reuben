//! The #247 P2 rewrite guard (ADR-0043 §7): every shipped control-block instrument, rewritten
//! to interface pipes + a surface doc, renders **bit-identically** to its pre-rewrite v2
//! original — at rest (a pipe with a declared default is the old literal) and under driven
//! input (the same gesture, sent to the old node address on the original and the pipe's
//! `/<name>/in` address on the rewrite). The pre-rewrite originals are snapshotted verbatim
//! under `tests/fixtures/pre-v3/`; voice/patch refs resolve against the live `instruments/`
//! tree (voices are not rewritten). This is the format_v2.rs shipped-corpus discipline.
//!
//! The corpus is "shipped instruments still sonically identical to their pre-v3 snapshots".
//! An instrument **exits** the guard when its sound is later evolved on purpose — the premise
//! ("the live file is a pure presentation rewrite of this snapshot") stops holding and no
//! honest fixture can restore it (a back-ported v2 doc would be a fabrication, not a
//! snapshot). First exit: groovebox, when its master chain gained the saturator + DJ filter +
//! volume knob (PR #266). Second exit: strum-harp, when its master chain gained the `/vtrim`
//! -> `/trim` headroom stage — its 8 resonator voices now pluck at full level and the raw sum
//! clipped without it, so the live file is louder *and* differently shaped than the snapshot
//! by design. Its sound is pinned instead by `tests/strum_harp.rs`, which asserts the level
//! directly. The v2→v3 *loader migration* stays covered by the remaining rows.
//!
//! Pipe naming (pinned here; the rewrite implements to it): the pipe keeps the *public*
//! control name; an internal node colliding with a minted pipe address is renamed
//! (ADR-0017's discipline, applied as a JSON-structural ref sweep):
//! - euclidean-drums: `tempo`, per channel (kick/snare/tom/hat): `<ch>_pulses`, `<ch>_steps`,
//!   `<ch>_rotation`, `<ch>_decay`, `<ch>_filter` (m2s → `/<ch>_filter_cv`), `<ch>_level`
//!   (m2s → `/<ch>_level_cv`)
//! - chord-player: `chord` (note pipe; chord node → `/triads`), `key`, `brightness`
//!   (m2s → `/brightness_cv`)
//! - strum-harp: `strum` (strum node → `/strummer`), `octaves`, `key`, `brightness`
//!   (m2s → `/brightness_cv`)
//!
//! (good-button, djfilter-demo, and granulator-demo were covered here until the library cull
//! removed them; the rewrite path stays pinned by euclidean-drums and chord-player.)

mod common;

use reuben_core::format::LoadWarning;
use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{load_instrument, AudioConfig, Registry};

const BLOCK: usize = 128;
const BLOCKS: usize = 40;

/// Text resources from the live `instruments/` tree (delegated to the shared `common::Dir`,
/// including its `FsResolver`-discipline rebase); samples synthesized in memory (the
/// surviving corpus is sample-free, but a future wav-carrying Toy renders audibly for free —
/// a real wav decode belongs to reuben-native, not core tests).
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
        common::Dir("instruments").resolve_text(source)
    }
    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        common::Dir("instruments").canonical(source, referrer)
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
fn euclidean_drums_rewrite_renders_bit_identical() {
    assert_rewritten("euclidean-drums");
}

#[test]
fn chord_player_rewrite_renders_bit_identical() {
    assert_rewritten("chord-player");
}
