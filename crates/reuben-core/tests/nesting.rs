//! Integration: general instrument-as-operator nesting (ADR-0034, nesting P4).
//!
//! The determinism acceptance criterion (ADR-0001): a nested patch renders **bit-identical** to
//! the hand-flattened equivalent instrument — inlining is an authoring concept with zero runtime
//! cost, so the rendered samples cannot differ by even one bit. Two reuses of one sub-instrument
//! must produce independent state (disjoint prefixes → disjoint nodes → no cross-talk), which the
//! bit-identical comparison proves too: shared oscillator state would advance phase twice per
//! block and diverge from the flattened twin immediately.

use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::{load, load_instrument, AudioConfig, Graph, Registry};

/// A single-oscillator sub-instrument exposing `freq` in / `audio` out.
const TONE: &str = r#"{
    "instrument": "tone",
    "interface": {
        "inputs":  { "freq": "/osc.freq" },
        "outputs": { "audio": "/osc.audio" }
    },
    "nodes": [ { "type": "oscillator", "address": "/osc" } ]
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
        reuben_core::message::Arg::Str("Saw".to_string()),
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
