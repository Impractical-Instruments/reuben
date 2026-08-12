//! Integration: general instrument-as-operator nesting (nesting P4).
//!
//! The determinism acceptance criterion: a nested patch renders **bit-identical** to
//! the hand-flattened equivalent instrument — inlining is an authoring concept with zero runtime
//! cost, so the rendered samples cannot differ by even one bit. Two reuses of one sub-instrument
//! must produce independent state (disjoint prefixes → disjoint nodes → no cross-talk), which the
//! bit-identical comparison proves too: shared oscillator state would advance phase twice per
//! block and diverge from the flattened twin immediately.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use reuben_core::descriptor::Port;
use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::SampleBuffer;
use reuben_core::vocab::pitch::{Note, Pitch};
use reuben_core::{AudioConfig, Descriptor, Graph, Io, Operator, Registry};
use reuben_document::resources::{ResolveError, ResourceResolver};
use reuben_document::{load, load_instrument};

/// A single-oscillator sub-instrument exposing `freq` in / `audio` out.
// A well-formed v1 child: its `audio` boundary output is also anonymously tapped (as every
// shipped v1 patch's was), so migration claims the entry — no boundary-only divergence
// warning (that accepted migration case is covered in format_v2.rs).
const TONE: &str = r#"{
    "instrument": "tone",
    "interface": {
        "inputs":  { "freq": "/osc.freq" },
        "outputs": { "audio": "/osc.audio" }
    },
    "nodes": [ { "type": "oscillator", "address": "/osc" } ],
    "outputs": [ { "node": "/osc", "port": "audio" } ]
}"#;

/// Hands back [`TONE`] for every source.
struct ToneResolver;

impl ResourceResolver for ToneResolver {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, _source: &str) -> Result<String, ResolveError> {
        Ok(TONE.to_string())
    }
}

/// Render `blocks` blocks of a graph with no input messages and return every sample.
fn render(graph: Graph, cfg: AudioConfig, blocks: usize) -> Vec<f32> {
    let mut plan = Plan::instantiate(graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; cfg.block_size];
    let mut all = Vec::with_capacity(blocks * cfg.block_size);
    for _ in 0..blocks {
        r.render_block(&mut plan, &[], &mut buf);
        all.extend_from_slice(&buf);
    }
    all
}

#[test]
fn nested_renders_bit_identical_to_hand_flattened() {
    // Two reuses of one sub-instrument, each with its own boundary literal, both tapped to master
    // through the face — against the same two oscillators written flat.
    const NESTED: &str = r#"{
        "instrument": "nested",
        "resources": { "tone": "tone.json" },
        "nodes": [
            { "type": "subpatch", "address": "/a", "patch": "tone", "inputs": { "freq": 220.0 } },
            { "type": "subpatch", "address": "/b", "patch": "tone", "inputs": { "freq": 330.0 } }
        ],
        "outputs": [ { "node": "/a", "port": "audio" }, { "node": "/b", "port": "audio" } ]
    }"#;
    const FLAT: &str = r#"{
        "instrument": "flat",
        "nodes": [
            { "type": "oscillator", "address": "/a/osc", "inputs": { "freq": 220.0 } },
            { "type": "oscillator", "address": "/b/osc", "inputs": { "freq": 330.0 } }
        ],
        "outputs": [ { "node": "/a/osc", "port": "audio" }, { "node": "/b/osc", "port": "audio" } ]
    }"#;

    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let loaded = load_instrument(NESTED, &reg, &ToneResolver).expect("load nested");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let nested = render(loaded.graph, cfg, 40);
    let flat = render(load(FLAT, &reg).expect("load flat"), cfg, 40);

    let peak = nested.iter().fold(0.0f32, |m, s| m.max(s.abs()));
    assert!(
        peak > 0.05,
        "nested patch rendered near-silence (peak {peak})"
    );
    assert_eq!(nested.len(), flat.len());
    for (i, (n, f)) in nested.iter().zip(&flat).enumerate() {
        assert_eq!(
            n.to_bits(),
            f.to_bits(),
            "sample {i} differs: nested {n} vs flat {f}"
        );
    }
}

#[test]
fn osc_message_reaches_a_spliced_node_shadowed_by_an_ancestor_address() {
    // `/a` is an ordinary parent node and the subpatch at `/a/sub` splices its child in as
    // `/a/sub/osc` — ancestor-prefixed addresses P4 manufactures systematically. An inbound
    // `/a/sub/osc/freq` prefix-matches `/a` first in plan order with no port match; routing must
    // keep scanning and deliver to the deeper node, not drop the message.
    // The wire from `/a` into the boundary pins the topo order: `/a` renders (and routes)
    // before the spliced `/a/sub/osc`, so the shadowing ancestor is genuinely scanned first.
    const NESTED: &str = r#"{
        "instrument": "nested",
        "resources": { "tone": "tone.json" },
        "nodes": [
            { "type": "oscillator", "address": "/a" },
            { "type": "subpatch", "address": "/a/sub", "patch": "tone",
              "inputs": { "freq": { "from": "/a.audio" } } }
        ],
        "outputs": [ { "node": "/a/sub", "port": "audio" } ]
    }"#;
    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let render_with = |msgs: &[reuben_core::message::Message]| {
        let loaded = load_instrument(NESTED, &reg, &ToneResolver).expect("load");
        let mut plan = Plan::instantiate(loaded.graph, cfg).expect("instantiate");
        let mut r = Renderer::new(&plan);
        let mut buf = vec![0.0f32; cfg.block_size];
        r.render_block(&mut plan, msgs, &mut buf);
        buf
    };

    let sine = render_with(&[]);
    let saw = render_with(&[reuben_core::message::Message::new(
        "/a/sub/osc/waveform",
        reuben_core::message::Arg::Str("Saw".into()),
        0,
    )]);
    assert_ne!(
        sine, saw,
        "the waveform message must reach /a/sub/osc through the /a prefix shadow"
    );
}

#[test]
fn boundary_wire_renders_bit_identical_to_direct_wire() {
    // The wire path through the face: parent node fed from `/a.audio` must be the same edge —
    // and the same audio — as wiring the inner oscillator directly.
    const NESTED: &str = r#"{
        "instrument": "nested",
        "resources": { "tone": "tone.json" },
        "nodes": [
            { "type": "subpatch", "address": "/a", "patch": "tone", "inputs": { "freq": 220.0 } },
            { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/a.audio" } } }
        ],
        "outputs": [ { "node": "/out", "port": "audio" } ]
    }"#;
    const FLAT: &str = r#"{
        "instrument": "flat",
        "nodes": [
            { "type": "oscillator", "address": "/a/osc", "inputs": { "freq": 220.0 } },
            { "type": "output", "address": "/out", "inputs": { "audio": { "from": "/a/osc.audio" } } }
        ],
        "outputs": [ { "node": "/out", "port": "audio" } ]
    }"#;

    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let loaded = load_instrument(NESTED, &reg, &ToneResolver).expect("load nested");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let nested = render(loaded.graph, cfg, 40);
    let flat = render(load(FLAT, &reg).expect("load flat"), cfg, 40);

    for (i, (n, f)) in nested.iter().zip(&flat).enumerate() {
        assert_eq!(
            n.to_bits(),
            f.to_bits(),
            "sample {i} differs: nested {n} vs flat {f}"
        );
    }
}

// --- P5: the one legal cross-kind bridge, F32 → F32Buffer (Value→Signal, ZOH),
// proven to *render* across the boundary in both orientations. The reverse (Buffer → F32,
// Signal→Value) is a hard load error — pinned in format.rs's unit matrix.

/// Serves whichever child JSON the test hands it.
struct Fixed(&'static str);

impl ResourceResolver for Fixed {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, _source: &str) -> Result<String, ResolveError> {
        Ok(self.0.to_string())
    }
}

#[test]
fn f32_value_zoh_materializes_into_the_nest() {
    // Parent F32 Value source (`add_f32_value.out`) wired into a boundary **Buffer** input: the
    // face inherits the inner `output.audio` Buffer type, the wire is the legal Value→Signal
    // bridge, and the sink ZOH-materializes the scalar — bit-identical to the flat twin, and
    // audibly nonzero (the DC level proves the value crossed, not a default-zero scratch).
    const GAIN_CHILD: &str = r#"{
        "instrument": "gain",
        "interface": {
            "inputs":  { "audio": "/out.audio" },
            "outputs": { "audio": "/out.audio" }
        },
        "nodes": [ { "type": "output", "address": "/out" } ],
        "outputs": [ { "node": "/out", "port": "audio" } ]
    }"#;
    const NESTED: &str = r#"{
        "instrument": "nested",
        "resources": { "g": "gain.json" },
        "nodes": [
            { "type": "add_f32_value", "address": "/num", "inputs": { "a": 0.25 } },
            { "type": "subpatch", "address": "/sub", "patch": "g",
              "inputs": { "audio": { "from": "/num.out" } } }
        ],
        "outputs": [ { "node": "/sub", "port": "audio" } ]
    }"#;
    const FLAT: &str = r#"{
        "instrument": "flat",
        "nodes": [
            { "type": "add_f32_value", "address": "/num", "inputs": { "a": 0.25 } },
            { "type": "output", "address": "/sub/out",
              "inputs": { "audio": { "from": "/num.out" } } }
        ],
        "outputs": [ { "node": "/sub/out", "port": "audio" } ]
    }"#;

    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let loaded = load_instrument(NESTED, &reg, &Fixed(GAIN_CHILD)).expect("load nested");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let nested = render(loaded.graph, cfg, 8);
    let flat = render(load(FLAT, &reg).expect("load flat"), cfg, 8);

    assert!(
        nested.iter().any(|s| *s != 0.0),
        "the scalar never reached the nested Buffer input"
    );
    assert_eq!(nested.len(), flat.len());
    for (i, (n, f)) in nested.iter().zip(&flat).enumerate() {
        assert_eq!(
            n.to_bits(),
            f.to_bits(),
            "sample {i} differs: nested {n} vs flat {f}"
        );
    }
}

#[test]
fn boundary_f32_output_zoh_materializes_into_the_parent() {
    // The other orientation: a boundary **F32 Value output** (the child's `add_f32_value.out`)
    // wired into a parent Buffer input. The face inherits the inner F32 type, so the same
    // Value→Signal bridge applies leaving the nest.
    const NUM_CHILD: &str = r#"{
        "instrument": "num",
        "interface": { "outputs": { "level": "/amt.out" } },
        "nodes": [ { "type": "add_f32_value", "address": "/amt", "inputs": { "a": 0.25 } } ]
    }"#;
    const NESTED: &str = r#"{
        "instrument": "nested",
        "resources": { "n": "num.json" },
        "nodes": [
            { "type": "subpatch", "address": "/sub", "patch": "n" },
            { "type": "output", "address": "/out",
              "inputs": { "audio": { "from": "/sub.level" } } }
        ],
        "outputs": [ { "node": "/out", "port": "audio" } ]
    }"#;
    const FLAT: &str = r#"{
        "instrument": "flat",
        "nodes": [
            { "type": "add_f32_value", "address": "/sub/amt", "inputs": { "a": 0.25 } },
            { "type": "output", "address": "/out",
              "inputs": { "audio": { "from": "/sub/amt.out" } } }
        ],
        "outputs": [ { "node": "/out", "port": "audio" } ]
    }"#;

    let cfg = AudioConfig::new(48_000.0, 256);
    let reg = Registry::builtin();

    let loaded = load_instrument(NESTED, &reg, &Fixed(NUM_CHILD)).expect("load nested");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    let nested = render(loaded.graph, cfg, 8);
    let flat = render(load(FLAT, &reg).expect("load flat"), cfg, 8);

    assert!(
        nested.iter().any(|s| *s != 0.0),
        "the boundary scalar never reached the parent Buffer input"
    );
    for (i, (n, f)) in nested.iter().zip(&flat).enumerate() {
        assert_eq!(
            n.to_bits(),
            f.to_bits(),
            "sample {i} differs: nested {n} vs flat {f}"
        );
    }
}

#[test]
fn mistyped_boundary_wire_fails_at_load_not_at_instantiate() {
    // The acceptance criterion stated end to end: a **well-typed inner graph** with a mistyped
    // boundary wire (Buffer → F32, Signal→Value) is a fatal LoadError in boundary terms — the
    // author never reaches Plan::instantiate, where the same defect would surface as a
    // FormMismatch naming the prefixed internals.
    const NUM_CHILD: &str = r#"{
        "instrument": "num",
        "interface": { "inputs": { "gain": "/amt.a" } },
        "nodes": [ { "type": "add_f32_value", "address": "/amt" } ]
    }"#;
    const NESTED: &str = r#"{
        "instrument": "nested",
        "resources": { "n": "num.json" },
        "nodes": [
            { "type": "oscillator", "address": "/osc" },
            { "type": "subpatch", "address": "/sub", "patch": "n",
              "inputs": { "gain": { "from": "/osc.audio" } } }
        ]
    }"#;
    let Err(err) = load_instrument(NESTED, &Registry::builtin(), &Fixed(NUM_CHILD)) else {
        panic!("mistyped boundary wire must be a fatal LoadError");
    };
    let msg = err.to_string();
    assert!(msg.contains("/sub.gain"), "boundary-named: {msg}");
    assert!(!msg.contains("/sub/amt"), "leaked internals: {msg}");
}

// ----------------------------------------------------------------------------------------------
// Reuse: a child document referenced N times is **built once**, and the other N-1 references are
// fresh-state copies of that one build. The observation seam is the registry's
// constructor: `make` runs once per node the loader *builds*, while `Operator::spawn` supplies
// every copy — so a counter on `make` separates the two without reaching inside the loader.
// ----------------------------------------------------------------------------------------------

/// Constructions of [`Probe`] via the registry. Process-wide, because a registry constructor is a
/// bare `fn` pointer with nowhere to hang per-test state — so every test that reads it holds
/// [`PROBE_LOCK`] for the duration, and the count is read as a delta either way.
static PROBE_BUILDS: AtomicUsize = AtomicUsize::new(0);

/// Serializes the tests that read [`PROBE_BUILDS`]; `cargo test` runs them on parallel threads.
static PROBE_LOCK: Mutex<()> = Mutex::new(());

/// A silent one-output operator that counts how many times the registry constructed it.
struct Probe;

impl Operator for Probe {
    fn descriptor() -> Descriptor {
        Descriptor {
            type_name: "probe_704",
            inputs: vec![],
            outputs: vec![Port::f32_buffer("out")],
            constants: vec![],
            resources: vec![],
        }
    }
    fn process(&mut self, _io: &mut Io) {}
    fn spawn(&self) -> Box<dyn Operator> {
        Box::new(Probe)
    }
}

/// [`Registry::builtin`] plus [`Probe`].
fn probe_registry() -> Registry {
    let mut reg = Registry::builtin();
    reg.register(
        || {
            PROBE_BUILDS.fetch_add(1, Ordering::Relaxed);
            Box::new(Probe)
        },
        Probe::descriptor(),
    );
    reg
}

#[test]
fn four_subpatch_reuses_build_the_child_once() {
    const CHILD: &str = r#"{
        "format_version": 2,
        "instrument": "child",
        "interface": { "outputs": { "out": { "from": "/p.out" } } },
        "nodes": [ { "type": "probe_704", "address": "/p" } ]
    }"#;
    const PARENT: &str = r#"{
        "format_version": 2,
        "instrument": "parent",
        "resources": { "c": "child.json" },
        "nodes": [
            { "type": "subpatch", "address": "/a", "patch": "c" },
            { "type": "subpatch", "address": "/b", "patch": "c" },
            { "type": "subpatch", "address": "/c", "patch": "c" },
            { "type": "subpatch", "address": "/d", "patch": "c" }
        ]
    }"#;

    let _serialized = PROBE_LOCK.lock().expect("probe lock");
    let before = PROBE_BUILDS.load(Ordering::Relaxed);
    let loaded = load_instrument(PARENT, &probe_registry(), &Fixed(CHILD)).expect("load");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);

    // The four reuses really are present — otherwise "built once" is trivially true because the
    // document degraded to nothing (the failure mode the construct bench also guards against).
    for address in ["/a/p", "/b/p", "/c/p", "/d/p"] {
        assert!(
            loaded.graph.find(address).is_some(),
            "{address} missing: the reuse dissolved instead of splicing"
        );
    }
    assert_eq!(
        PROBE_BUILDS.load(Ordering::Relaxed) - before,
        1,
        "four reuses of one child must build it once and copy it three times"
    );
}

/// Counts `resolve_text` calls alongside [`Fixed`]'s single answer — the voice pass's per-copy
/// re-read is visible here and nowhere else, since the parse cache alone never covered it.
struct CountingFixed(&'static str, AtomicUsize);

impl ResourceResolver for CountingFixed {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, _source: &str) -> Result<String, ResolveError> {
        self.1.fetch_add(1, Ordering::Relaxed);
        Ok(self.0.to_string())
    }
}

#[test]
fn a_thirty_two_voice_pool_builds_its_patch_once() {
    // The voice pass is the other O(reuses) build: a Voicer hosts `voices` copies of one patch.
    // Unlike the subpatch pass it never even shared a *parse*, so a 32-voice pool used to read,
    // parse and build the same source 32 times.
    const VOICE: &str = r#"{
        "format_version": 2,
        "instrument": "voice",
        "interface": {
            "inputs": {
                "freq": { "type": "f32", "default": 440.0, "min": 20.0, "max": 20000.0 },
                "gate": { "type": "f32", "default": 0.0, "min": 0.0, "max": 1.0 }
            },
            "outputs": { "audio": { "from": "/p.out" } }
        },
        "nodes": [ { "type": "probe_704", "address": "/p" } ]
    }"#;
    const HOST: &str = r#"{
        "format_version": 2,
        "instrument": "host",
        "resources": { "v": "voice.json" },
        "interface": { "outputs": { "out": { "from": "/out.audio" } } },
        "nodes": [
            { "type": "voicer", "address": "/voicer", "voice": "v", "config": { "voices": 32 } },
            { "type": "output", "address": "/out",
              "inputs": { "audio": { "from": "/voicer.audio" } } }
        ]
    }"#;

    let _serialized = PROBE_LOCK.lock().expect("probe lock");
    let resolver = CountingFixed(VOICE, AtomicUsize::new(0));
    let before = PROBE_BUILDS.load(Ordering::Relaxed);
    let loaded = load_instrument(HOST, &probe_registry(), &resolver).expect("load");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);

    assert_eq!(
        PROBE_BUILDS.load(Ordering::Relaxed) - before,
        1,
        "a 32-voice pool must build its patch once and copy it 31 times"
    );
    assert_eq!(
        resolver.1.load(Ordering::Relaxed),
        1,
        "and read the source once"
    );

    // The pool really is 32 voices deep — a patch that failed to bind would also "build once".
    let mut plan = Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, 64))
        .expect("instantiate the pool");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; 64];
    r.render_block(&mut plan, &[], &mut buf);
}

/// Serves a named set of child documents, so a test can nest one inside another.
struct Library(&'static [(&'static str, &'static str)]);

impl ResourceResolver for Library {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        self.0
            .iter()
            .find(|(name, _)| *name == source)
            .map(|(_, doc)| doc.to_string())
            .ok_or_else(|| ResolveError::NotFound(source.to_string()))
    }
}

/// The shipped voice patch, plus a section that hosts a pool of it behind an `audio` face.
const POLY_LIBRARY: &[(&str, &str)] = &[
    (
        "voice.json",
        include_str!("../../../instruments/voices/default-voice.json"),
    ),
    (
        "section.json",
        r#"{
            "format_version": 2,
            "instrument": "section",
            "resources": { "v": "voice.json" },
            "interface": { "outputs": { "audio": { "from": "/voicer.audio" } } },
            "nodes": [
                { "type": "voicer", "address": "/voicer", "voice": "v",
                  "config": { "voices": 2 } }
            ]
        }"#,
    ),
];

#[test]
fn a_reused_section_keeps_the_voices_its_voicer_hosts() {
    // A Voicer's bound voice graphs are state a copy cannot drop: they are the pool it renders.
    // A section is exactly the shape a song repeats — so the *second* reuse, the one served as a
    // copy rather than a build, is what gets played here. Silence would be the failure.
    const SONG: &str = r#"{
        "format_version": 2,
        "instrument": "song",
        "resources": { "s": "section.json" },
        "interface": { "outputs": { "out": { "from": "/out.audio" } } },
        "nodes": [
            { "type": "subpatch", "address": "/a", "patch": "s" },
            { "type": "subpatch", "address": "/b", "patch": "s" },
            { "type": "output", "address": "/out",
              "inputs": { "audio": { "from": "/b.audio" } } }
        ]
    }"#;

    let loaded = load_instrument(SONG, &Registry::builtin(), &Library(POLY_LIBRARY)).expect("load");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);

    let cfg = AudioConfig::new(48_000.0, 128);
    let mut plan = Plan::instantiate(loaded.graph, cfg).expect("instantiate");
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; cfg.block_size];
    let note = Message::new("/b/voicer/notes", Note::new(Pitch::Degree(0), 1.0), 0);
    r.render_block(&mut plan, &[note], &mut buf);

    assert!(
        buf.iter().any(|s| *s != 0.0),
        "the second reuse of a voicer-hosting section rendered silence: its pool was dropped"
    );
}

#[test]
fn a_source_one_reference_already_built_is_not_read_again_for_another() {
    // `/c` is a section hosting a two-voice pool of `voice.json`; `/v` then names that same
    // document directly as a subpatch. By the time `/v` is reached the source has been built and
    // cached, so the read, the parse and the build are all already paid — asking the resolver again
    // only to discard the answer is work this change exists to remove, and it is also the one way
    // two references to one source could still disagree about whether it exists.
    const VOICE: &str = r#"{
        "format_version": 2,
        "instrument": "voice",
        "interface": {
            "inputs": {
                "freq": { "type": "f32", "default": 440.0, "min": 20.0, "max": 20000.0 },
                "gate": { "type": "f32", "default": 0.0, "min": 0.0, "max": 1.0 }
            },
            "outputs": { "audio": { "from": "/p.out" } }
        },
        "nodes": [ { "type": "probe_704", "address": "/p" } ]
    }"#;
    const SECTION: &str = r#"{
        "format_version": 2,
        "instrument": "section",
        "resources": { "v": "voice.json" },
        "interface": { "outputs": { "audio": { "from": "/voicer.audio" } } },
        "nodes": [
            { "type": "voicer", "address": "/voicer", "voice": "v", "config": { "voices": 2 } }
        ]
    }"#;
    const SONG: &str = r#"{
        "format_version": 2,
        "instrument": "song",
        "resources": { "s": "section.json", "v": "voice.json" },
        "nodes": [
            { "type": "subpatch", "address": "/c", "patch": "s" },
            { "type": "subpatch", "address": "/v", "patch": "v" }
        ]
    }"#;

    struct Counting(AtomicUsize);
    impl ResourceResolver for Counting {
        fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(source.to_string()))
        }
        fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
            self.0.fetch_add(1, Ordering::Relaxed);
            match source {
                "section.json" => Ok(SECTION.to_string()),
                "voice.json" => Ok(VOICE.to_string()),
                other => Err(ResolveError::NotFound(other.to_string())),
            }
        }
    }

    let _serialized = PROBE_LOCK.lock().expect("probe lock");
    let resolver = Counting(AtomicUsize::new(0));
    let loaded = load_instrument(SONG, &probe_registry(), &resolver).expect("load");
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert!(
        loaded.graph.find("/v/p").is_some(),
        "the direct reuse spliced"
    );

    assert_eq!(
        resolver.0.load(Ordering::Relaxed),
        2,
        "one read per distinct source: section.json and voice.json"
    );
}

#[test]
fn a_child_whose_sample_is_missing_is_still_built_once() {
    // A build that lost a `patch`/`voice` reference is not cached, because those are the
    // references the cycle guard walks. A missing **sample** is a different thing: it is not a
    // graph, cannot re-enter the load, and cannot hide a cycle — and a library child whose sample
    // the user has not installed is ordinary, not exotic. So it still caches, and each site still
    // gets its own copy of the warning.
    const CHILD: &str = r#"{
        "format_version": 2,
        "instrument": "child",
        "resources": { "kick": "kick.wav" },
        "interface": { "outputs": { "out": { "from": "/p.out" } } },
        "nodes": [
            { "type": "probe_704", "address": "/p" },
            { "type": "sample", "address": "/s", "sample": "kick" }
        ]
    }"#;
    const PARENT: &str = r#"{
        "format_version": 2,
        "instrument": "parent",
        "resources": { "c": "child.json" },
        "nodes": [
            { "type": "subpatch", "address": "/a", "patch": "c" },
            { "type": "subpatch", "address": "/b", "patch": "c" },
            { "type": "subpatch", "address": "/c", "patch": "c" }
        ]
    }"#;

    let _serialized = PROBE_LOCK.lock().expect("probe lock");
    let before = PROBE_BUILDS.load(Ordering::Relaxed);
    let loaded = load_instrument(PARENT, &probe_registry(), &Fixed(CHILD)).expect("non-fatal");

    assert_eq!(
        PROBE_BUILDS.load(Ordering::Relaxed) - before,
        1,
        "a dead sample must not cost the child its cache"
    );
    let sites = loaded
        .warnings
        .iter()
        .filter(|w| matches!(w, reuben_document::LoadWarning::Nested { .. }))
        .count();
    assert_eq!(
        sites, 3,
        "each site keeps its own warning: {:?}",
        loaded.warnings
    );
}
