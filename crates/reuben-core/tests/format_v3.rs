//! Format v3: presentation decouples from the instrument document.
//!
//! v3 removes the two retired presentation carriers — the per-node `control` block
//! (the per-node `control` block) and `label`/`widget` on interface pipes (as amended) — with the
//! ignore-with-warning migration: a v2 document (or a v3 document still
//! carrying leftovers) loads, the loader drops the retired fields and emits a `LoadWarning`
//! naming each, and save writes clean v3. Sound is unaffected by construction (the engine
//! never read any of them) — asserted here bit-identically.

use reuben_core::format::LoadWarning;
use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::{ResolveError, ResourceResolver, SampleBuffer};
use reuben_core::{load_instrument, AudioConfig, NormalizedDoc, Registry};

const BLOCK: usize = 128;
const BLOCKS: usize = 20;

/// These fixtures reference no samples/patches, so every resolve is a miss.
struct NoResources;
impl ResourceResolver for NoResources {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }
}

/// Everything a render makes observable past the boundary (the format_v2.rs discipline):
/// master channels, outbound messages, captured Value interface outputs.
#[derive(Debug, PartialEq)]
struct Rendered {
    channels: Vec<Vec<f32>>,
    outbound: Vec<(usize, Message)>,
    captured: Vec<Vec<f32>>,
}

fn render(top: &str, messages: impl Fn(usize) -> Vec<Message>) -> Rendered {
    let loaded = load_instrument(top, &Registry::builtin(), &NoResources).expect("load");
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

/// A v2 document with a `control` block: an audible oscillator→filter chain whose filter node
/// carries UI metadata the engine never read.
const V2_WITH_CONTROL: &str = r#"{
  "format_version": 2,
  "instrument": "v3-control-strip",
  "interface": {
    "inputs": { "tone": { "type": "f32_buffer", "default": 800, "min": 20, "max": 8000 } },
    "outputs": { "out": { "from": "/filter.audio" } }
  },
  "nodes": [
    { "type": "oscillator", "address": "/osc", "inputs": { "freq": 110.0 } },
    { "type": "filter", "address": "/filter",
      "inputs": { "audio": { "from": "/osc" }, "cutoff": { "from": "/tone" } },
      "control": { "label": "Filter", "widget": "fader" } }
  ]
}"#;

/// The same document with the `control` block already stripped by hand.
const V2_WITHOUT_CONTROL: &str = r#"{
  "format_version": 2,
  "instrument": "v3-control-strip",
  "interface": {
    "inputs": { "tone": { "type": "f32_buffer", "default": 800, "min": 20, "max": 8000 } },
    "outputs": { "out": { "from": "/filter.audio" } }
  },
  "nodes": [
    { "type": "oscillator", "address": "/osc", "inputs": { "freq": 110.0 } },
    { "type": "filter", "address": "/filter",
      "inputs": { "audio": { "from": "/osc" }, "cutoff": { "from": "/tone" } } }
  ]
}"#;

#[test]
fn v2_control_block_is_ignored_with_a_warning_and_save_strips_it() {
    let loaded = load_instrument(V2_WITH_CONTROL, &Registry::builtin(), &NoResources)
        .expect("a control-carrying v2 document keeps loading (ignore-with-warning)");
    assert!(
        loaded.warnings.iter().map(flat).any(|w| matches!(
            w,
            LoadWarning::DeprecatedControlBlock { node } if node == "/filter"
        )),
        "expected a DeprecatedControlBlock warning naming /filter, got: {:?}",
        loaded.warnings
    );

    let doc = NormalizedDoc::from_json(V2_WITH_CONTROL, &Registry::builtin(), None).expect("parse");
    let saved = doc.to_json_pretty();
    assert!(
        !saved.contains("\"control\""),
        "save must strip the retired control block, got:\n{saved}"
    );
    assert!(
        saved.contains("\"format_version\": 3"),
        "save writes v3, got:\n{saved}"
    );
}

/// A v2 document whose pipes carry retired presentation (`label`/`widget`) alongside the
/// quantity contract that stays (`type`/`default`/`min`/`max`/`curve`/`unit`).
const V2_WITH_PIPE_PRESENTATION: &str = r#"{
  "format_version": 2,
  "instrument": "v3-pipe-strip",
  "interface": {
    "inputs": { "tone": { "type": "f32_buffer", "default": 800, "min": 20, "max": 8000,
                          "curve": "exp", "unit": "Hz",
                          "label": "Tone", "widget": "radial" } },
    "outputs": { "out": { "from": "/filter.audio", "label": "Out", "widget": "meter" } }
  },
  "nodes": [
    { "type": "oscillator", "address": "/osc", "inputs": { "freq": 110.0 } },
    { "type": "filter", "address": "/filter",
      "inputs": { "audio": { "from": "/osc" }, "cutoff": { "from": "/tone" } } }
  ]
}"#;

/// The same document with `label`/`widget` already stripped by hand.
const V2_WITHOUT_PIPE_PRESENTATION: &str = r#"{
  "format_version": 2,
  "instrument": "v3-pipe-strip",
  "interface": {
    "inputs": { "tone": { "type": "f32_buffer", "default": 800, "min": 20, "max": 8000,
                          "curve": "exp", "unit": "Hz" } },
    "outputs": { "out": { "from": "/filter.audio" } }
  },
  "nodes": [
    { "type": "oscillator", "address": "/osc", "inputs": { "freq": 110.0 } },
    { "type": "filter", "address": "/filter",
      "inputs": { "audio": { "from": "/osc" }, "cutoff": { "from": "/tone" } } }
  ]
}"#;

#[test]
fn v2_pipe_label_and_widget_are_ignored_with_warnings_and_save_strips_them() {
    let loaded = load_instrument(
        V2_WITH_PIPE_PRESENTATION,
        &Registry::builtin(),
        &NoResources,
    )
    .expect("a label/widget-carrying v2 document keeps loading (ignore-with-warning)");
    let dropped: Vec<(&str, &str)> = loaded
        .warnings
        .iter()
        .map(flat)
        .filter_map(|w| match w {
            LoadWarning::DeprecatedPipePresentation { name, field } => {
                Some((name.as_str(), *field))
            }
            _ => None,
        })
        .collect();
    for expected in [
        ("tone", "label"),
        ("tone", "widget"),
        ("out", "label"),
        ("out", "widget"),
    ] {
        assert!(
            dropped.contains(&expected),
            "expected a DeprecatedPipePresentation warning for {expected:?}, got: {dropped:?}"
        );
    }

    let doc = NormalizedDoc::from_json(V2_WITH_PIPE_PRESENTATION, &Registry::builtin(), None)
        .expect("parse");
    let saved = doc.to_json_pretty();
    assert!(
        !saved.contains("\"label\"") && !saved.contains("\"widget\""),
        "save must strip retired pipe presentation, got:\n{saved}"
    );
    for kept in ["\"curve\"", "\"unit\"", "\"min\"", "\"max\"", "\"default\""] {
        assert!(
            saved.contains(kept),
            "the quantity contract ({kept}) must survive the strip, got:\n{saved}"
        );
    }
}

#[test]
fn pipe_presentation_carrying_doc_renders_bit_identical_to_the_stripped_doc() {
    let with = render(V2_WITH_PIPE_PRESENTATION, |_| Vec::new());
    let without = render(V2_WITHOUT_PIPE_PRESENTATION, |_| Vec::new());
    assert_nonsilent(&with, "pipe-presentation-carrying doc");
    assert_bit_identical(&with, &without, "pipe label/widget strip");
}

#[test]
fn v3_stamped_doc_with_leftovers_degrades_identically() {
    // Ignore-with-warning is not gated on the stamp — a v3 document still
    // carrying retired fields loads, warns, and strips exactly like a v2 one.
    let v3 = V2_WITH_CONTROL.replace("\"format_version\": 2", "\"format_version\": 3");
    let loaded = load_instrument(&v3, &Registry::builtin(), &NoResources)
        .expect("a v3-stamped document with leftovers keeps loading");
    assert!(
        loaded.warnings.iter().map(flat).any(|w| matches!(
            w,
            LoadWarning::DeprecatedControlBlock { node } if node == "/filter"
        )),
        "expected the same DeprecatedControlBlock warning under a v3 stamp, got: {:?}",
        loaded.warnings
    );
    let doc = NormalizedDoc::from_json(&v3, &Registry::builtin(), None).expect("parse");
    assert!(!doc.to_json_pretty().contains("\"control\""));
}

#[test]
fn v1_doc_migrates_through_to_v3_and_save_writes_v3() {
    // The whole chain: absent format_version (= v1) → target-form migration →
    // presentation strip → stamped v3 on save.
    let v1 = r#"{"instrument":"t","interface":{
        "inputs":{"tone":{"target":"/filter.cutoff","label":"Tone"}},
        "outputs":{"out":"/filter.audio"}},
    "nodes":[{"type":"filter","address":"/filter","inputs":{"cutoff":4000.0}}]}"#;
    let doc = NormalizedDoc::from_json(v1, &Registry::builtin(), None).expect("v1 keeps loading");
    let saved = doc.to_json_pretty();
    assert!(
        saved.contains("\"format_version\": 3"),
        "a migrated v1 doc saves as v3, got:\n{saved}"
    );
    assert!(
        !saved.contains("\"label\""),
        "the v1 label carried into the pipe is stripped on the way to v3, got:\n{saved}"
    );
}

#[test]
fn control_block_carrying_doc_renders_bit_identical_to_the_stripped_doc() {
    // The P1 guard: `control` was an opaque passthrough the engine never read, so dropping it
    // is render-safe by construction — asserted, not assumed.
    let with = render(V2_WITH_CONTROL, |_| Vec::new());
    let without = render(V2_WITHOUT_CONTROL, |_| Vec::new());
    assert_nonsilent(&with, "control-carrying doc");
    assert_bit_identical(&with, &without, "control block strip");
}
