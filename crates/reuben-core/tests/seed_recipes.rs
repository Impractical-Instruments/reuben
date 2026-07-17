//! The R3 seed-recipe guard (ADR-0057 §5): euclidean-drums, re-expressed through the seed
//! recipes (`voices/kick-voice.json` ×2 — the tom is the same body overridden —
//! `voices/snare-voice.json`, `voices/hat-voice.json`, each through `patches/dj-strip.json`),
//! renders **bit-identically** to the pre-recipe inline version — at rest and under driven
//! input on the unchanged top-level pipes. The pre-recipe originals are snapshotted verbatim
//! under `tests/fixtures/pre-recipes/`; this is the format_v3_rewrite.rs shipped-corpus
//! discipline (ADR-0026), applied to the recipe re-expression instead of a format rewrite.
//!
//! Alongside the headline assertion:
//! - each promoted drum voice (baked literals → defaulted interface pipes, rebuilt on the
//!   nested `shaped-vca`) renders bit-identically to its pre-promotion snapshot under the
//!   same gate gestures — the promotion is a pure refactor, so the voicer hosts (groovebox)
//!   hear nothing;
//! - every seed document validates through the real engine load path (`load_instrument` +
//!   `Plan::instantiate`), warning-free except the sanctioned top-level bare-`audio`
//!   `UnboundInputPipe` on the two processors (the same shape `patches/space.json` has —
//!   nothing can feed a bare signal pipe at top level).

use reuben_core::format::LoadWarning;
use reuben_core::message::{Arg, Message};
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::{load_instrument, AudioConfig, Registry};

const BLOCK: usize = 128;
const BLOCKS: usize = 80;

/// Text resources from a repo directory (the corpus is sample-free). Keys are relative to the
/// root the resolver is built with — `Dir("instruments")` mirrors loading a top-level
/// instrument, `Dir("instruments/voices")` mirrors `reuben play` on a voice document (the
/// `FsResolver::for_instrument` base).
struct Dir(&'static str);
impl ResourceResolver for Dir {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        let path = format!("{}/../../{}/{source}", env!("CARGO_MANIFEST_DIR"), self.0);
        std::fs::read_to_string(&path).map_err(|e| ResolveError::NotFound(format!("{path}: {e}")))
    }
    /// Per-document rebase (the `FsResolver` discipline, ADR-0034 §1): a nested document's own
    /// references (kick-voice.json's `shaped-vca.json`) resolve next to *it*, keys staying
    /// root-relative.
    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        match referrer.and_then(|r| r.rsplit_once('/')) {
            Some((dir, _)) => format!("{dir}/{source}"),
            None => source.to_string(),
        }
    }
}

fn shipped(dir: &'static str, name: &str) -> String {
    Dir(dir)
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
    resolver: &Dir,
    blocks: usize,
    messages: impl Fn(usize) -> Vec<Message>,
) -> (Rendered, Vec<LoadWarning>) {
    let loaded = load_instrument(top, &Registry::builtin(), resolver).expect("load");
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

fn f32_msg(addr: &str, v: f32, frame: usize) -> Message {
    Message::new(addr, Arg::F32(v), frame)
}

/// The driven gesture stream for the euclid comparison. Every address is a **top-level
/// interface pipe**, unchanged by the re-expression, so one stream drives both sides —
/// including the four DECAY knobs, whose values now travel through two nested boundary faces
/// (top pipe → body face → shaped-vca face → envelope input).
fn euclid_gestures(b: usize) -> Vec<Message> {
    match b {
        4 => vec![f32_msg("/kick_filter/in", -0.5, 21)],
        8 => vec![f32_msg("/tempo/in", 100.0, 3)],
        10 => vec![
            f32_msg("/kick_decay/in", 0.4, 10),
            f32_msg("/tom_decay/in", 0.35, 40),
        ],
        14 => vec![
            f32_msg("/kick_pulses/in", 5.0, 60),
            f32_msg("/snare_decay/in", 0.05, 60),
            f32_msg("/hat_level/in", 0.2, 60),
        ],
        22 => vec![
            f32_msg("/hat_filter/in", 0.8, 5),
            f32_msg("/snare_filter/in", -0.9, 70),
            f32_msg("/tom_filter/in", 0.4, 33),
        ],
        36 => vec![
            f32_msg("/hat_decay/in", 0.25, 0),
            f32_msg("/snare_level/in", 0.9, 90),
            f32_msg("/tom_level/in", 0.3, 100),
        ],
        52 => vec![
            f32_msg("/kick_rotation/in", 2.0, 0),
            f32_msg("/hat_pulses/in", 11.0, 15),
        ],
        _ => Vec::new(),
    }
}

/// ADR-0057 §5's named acceptance: the re-expressed document renders bit-identical to the
/// pre-recipe inline version, and its recipe references resolve warning-free.
#[test]
fn euclidean_drums_re_expression_renders_bit_identical() {
    let fixture = include_str!("fixtures/pre-recipes/euclidean-drums.json");
    let (pre, _) = render(fixture, &Dir("instruments"), BLOCKS, euclid_gestures);
    let (post, warnings) = render(
        &shipped("instruments", "euclidean-drums.json"),
        &Dir("instruments"),
        BLOCKS,
        euclid_gestures,
    );
    assert_nonsilent(&pre, "euclidean-drums");
    assert!(
        warnings.is_empty(),
        "euclidean-drums: recipe references must resolve clean, got: {warnings:?}"
    );
    assert_bit_identical(
        &pre,
        &post,
        "euclidean-drums: recipe re-expression vs pre-recipe inline",
    );
}

/// The gate gesture stream for the voice comparisons: on/off cycles hitting attack, decay,
/// the sustain-0 tail, release, and a mid-block retrigger.
fn gate_gestures(b: usize) -> Vec<Message> {
    match b {
        2 => vec![f32_msg("/gate/in", 1.0, 0)],
        6 => vec![f32_msg("/gate/in", 0.0, 0)],
        10 => vec![f32_msg("/gate/in", 1.0, 64)],
        11 => vec![f32_msg("/gate/in", 0.0, 32)],
        20 => vec![f32_msg("/gate/in", 1.0, 5)],
        30 => vec![f32_msg("/gate/in", 0.0, 5)],
        _ => Vec::new(),
    }
}

/// One promoted drum voice: unchanged at its promoted defaults — every former baked literal
/// became a pipe defaulting to that literal, and the drum body was rebuilt on the nested
/// `shaped-vca` — so a voicer host (groovebox) hears exactly the pre-promotion sound.
fn assert_promoted(name: &str, fixture: &str) {
    let (pre, _) = render(fixture, &Dir("instruments/voices"), 40, gate_gestures);
    let (post, warnings) = render(
        &shipped("instruments/voices", &format!("{name}.json")),
        &Dir("instruments/voices"),
        40,
        gate_gestures,
    );
    assert_nonsilent(&pre, name);
    assert!(
        warnings.is_empty(),
        "{name}: promoted voice must load clean, got: {warnings:?}"
    );
    assert_bit_identical(&pre, &post, &format!("{name}: promotion vs baked literals"));
}

#[test]
fn promoted_kick_voice_renders_bit_identical() {
    assert_promoted(
        "kick-voice",
        include_str!("fixtures/pre-recipes/kick-voice.json"),
    );
}

#[test]
fn promoted_snare_voice_renders_bit_identical() {
    assert_promoted(
        "snare-voice",
        include_str!("fixtures/pre-recipes/snare-voice.json"),
    );
}

#[test]
fn promoted_hat_voice_renders_bit_identical() {
    assert_promoted(
        "hat-voice",
        include_str!("fixtures/pre-recipes/hat-voice.json"),
    );
}

/// Every seed document loads and instantiates through the real engine load path — the single
/// validation authority (ADR-0045) — from its own directory, the `FsResolver::for_instrument`
/// base `reuben play` would use. `bare_audio` marks the two processors whose bare `audio`
/// signal pipe legitimately warns unfed at top level.
#[test]
fn every_seed_validates_through_the_engine_load_path() {
    let seeds: &[(&str, &str, bool)] = &[
        ("instruments/voices", "shaped-vca.json", true),
        ("instruments/voices", "kick-voice.json", false),
        ("instruments/voices", "snare-voice.json", false),
        ("instruments/voices", "hat-voice.json", false),
        ("instruments/patches", "dj-strip.json", true),
        ("instruments/patches", "good-button.json", false),
    ];
    for &(dir, name, bare_audio) in seeds {
        let loaded = load_instrument(&shipped(dir, name), &Registry::builtin(), &Dir(dir))
            .unwrap_or_else(|e| panic!("{name}: seed must load, got {e:?}"));
        let unexpected: Vec<_> = loaded
            .warnings
            .iter()
            .filter(|w| {
                !(bare_audio
                    && matches!(w, LoadWarning::UnboundInputPipe { name } if name == "audio"))
            })
            .collect();
        assert!(
            unexpected.is_empty(),
            "{name}: seed must validate warning-free, got: {unexpected:?}"
        );
        Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, BLOCK))
            .unwrap_or_else(|e| panic!("{name}: seed must instantiate, got {e:?}"));
    }
}
