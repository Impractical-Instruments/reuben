//! `layout`: the editor's canvas position on a node and on an interface pipe.
//!
//! The engine never reads it. Two documents differing only in `layout` build the same `Plan` and
//! play bit-identically, and every save carries it back out unchanged — at the top level and in
//! every nested voice and subpatch document.

mod common;

use common::Dir;
use reuben_core::message::Message;
use reuben_core::plan::Plan;
use reuben_core::render::Renderer;
use reuben_core::resources::SampleBuffer;
use reuben_core::{AudioConfig, Registry};
use reuben_document::edit;
use reuben_document::projection::{Projector, Selection};
use reuben_document::resources::{ResolveError, ResourceResolver};
use reuben_document::{load_instrument, MemoryResolver, NormalizedDoc};
use serde_json::{json, Value};

/// Self-playing, and nests two levels deep: three Voicers each host a drum voice, and the kick
/// voice inlines a `shaped-vca` subpatch.
const GROOVEBOX: &str = include_str!("../../../instruments/groovebox.json");
const NESTED: &[&str] = &[
    "voices/kick-voice.json",
    "voices/snare-voice.json",
    "voices/hat-voice.json",
    "voices/shaped-vca.json",
];

/// A distinct position per entry, so a save that swapped two entries' layouts would be caught.
/// `0.1` is not exact in binary: it proves the `f32` survives the JSON text round trip.
fn position(i: usize) -> Value {
    json!({ "x": i as f32 * 40.5 + 0.1, "y": -(i as f32) * 12.25 })
}

/// Give every node and every v2 interface entry of `text` a `layout`.
fn with_layout(text: &str) -> String {
    let mut doc: Value = serde_json::from_str(text).expect("fixture is JSON");
    let mut i = 0;
    for node in doc["nodes"].as_array_mut().expect("nodes") {
        node["layout"] = position(i);
        i += 1;
    }
    if let Some(iface) = doc.get_mut("interface") {
        for side in ["inputs", "outputs"] {
            for entry in iface
                .get_mut(side)
                .and_then(Value::as_object_mut)
                .into_iter()
                .flat_map(|m| m.values_mut())
            {
                entry["layout"] = position(i);
                i += 1;
            }
        }
    }
    serde_json::to_string_pretty(&doc).expect("serialize")
}

/// Every `layout` in `text`, keyed by where it sits, in document order — read back as the `f32`s
/// the format holds, since the JSON spelling of one `f32` is not unique.
fn layouts(text: &str) -> Vec<(String, (f32, f32))> {
    let xy = |l: &Value| {
        (
            l["x"].as_f64().unwrap() as f32,
            l["y"].as_f64().unwrap() as f32,
        )
    };
    let doc: Value = serde_json::from_str(text).expect("JSON");
    let mut out = Vec::new();
    for node in doc["nodes"].as_array().expect("nodes") {
        if let Some(l) = node.get("layout") {
            out.push((node["address"].as_str().unwrap().to_string(), xy(l)));
        }
    }
    if let Some(iface) = doc.get("interface") {
        for side in ["inputs", "outputs"] {
            for (name, entry) in iface
                .get(side)
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
            {
                if let Some(l) = entry.get("layout") {
                    out.push((format!("{side}.{name}"), xy(l)));
                }
            }
        }
    }
    out
}

/// The library resolver, with a `layout` added to every nested document it hands the loader.
struct Laid(Dir);

impl ResourceResolver for Laid {
    fn resolve(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.0.resolve(source)
    }

    fn resolve_text(&self, source: &str) -> Result<String, ResolveError> {
        self.0.resolve_text(source).map(|t| with_layout(&t))
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.0.canonical(source, referrer)
    }
}

/// Everything about a `Plan`'s shape a render depends on, in plan order. (`Plan` holds boxed
/// Operators, so it has no `PartialEq` of its own.)
fn shape(plan: &Plan) -> Vec<String> {
    let mut out = vec![format!("buffers={}", plan.num_buffers)];
    out.extend(plan.nodes().iter().map(|n| {
        format!(
            "{} {} in={:?} kinds={:?} mat={:?}",
            n.address, n.descriptor.type_name, n.inputs, n.input_kinds, n.materialize
        )
    }));
    out.extend(
        plan.output_taps
            .iter()
            .map(|t| format!("tap ch={:?} buf={:?}", t.channel, t.buffer)),
    );
    out
}

fn instantiate(top: &str, resolver: &dyn ResourceResolver) -> Plan {
    let loaded = load_instrument(top, &Registry::builtin(), resolver).expect("load");
    assert!(
        loaded.warnings.is_empty(),
        "layout must not warn: {:?}",
        loaded.warnings
    );
    Plan::instantiate(loaded.graph, AudioConfig::new(48_000.0, 256)).expect("instantiate")
}

fn render(mut plan: Plan) -> Vec<f32> {
    let mut r = Renderer::new(&plan);
    let mut buf = vec![0.0f32; 256];
    let mut out = Vec::new();
    let no_msgs: Vec<Message> = Vec::new();
    // ~1 s at 48 kHz: several steps of every track.
    for _ in 0..188 {
        r.render_block(&mut plan, &no_msgs, &mut buf);
        out.extend_from_slice(&buf);
    }
    out
}

#[test]
fn layout_changes_nothing_the_engine_builds_or_plays() {
    let laid_top = with_layout(GROOVEBOX);
    assert!(!layouts(&laid_top).is_empty());
    for path in NESTED {
        let text = Dir("instruments")
            .resolve_text(path)
            .expect("nested fixture");
        assert!(
            !layouts(&with_layout(&text)).is_empty(),
            "{path}: nothing to lay out"
        );
    }

    let bare = instantiate(GROOVEBOX, &Dir("instruments"));
    let laid = instantiate(&laid_top, &Laid(Dir("instruments")));
    assert_eq!(shape(&bare), shape(&laid), "layout changed the Plan");

    let (bare, laid) = (render(bare), render(laid));
    assert!(bare.iter().any(|s| s.abs() > 0.05), "fixture is silent");
    assert_eq!(bare, laid, "layout changed the sound");
}

#[test]
fn layout_survives_load_and_save_in_every_document() {
    let registry = Registry::builtin();
    let mut texts = vec![("groovebox.json".to_string(), GROOVEBOX.to_string())];
    for path in NESTED {
        let text = Dir("instruments")
            .resolve_text(path)
            .expect("nested fixture");
        texts.push((path.to_string(), text));
    }
    for (path, text) in texts {
        let laid = with_layout(&text);
        let doc = NormalizedDoc::from_json(&laid, &registry, Some(&Dir("instruments")))
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        let saved = doc.to_json_pretty();
        assert_eq!(layouts(&saved), layouts(&laid), "{path}: layout drifted");
        let reparsed = NormalizedDoc::from_json(&saved, &registry, Some(&Dir("instruments")))
            .unwrap_or_else(|e| panic!("{path}: {e}"));
        assert_eq!(doc, reparsed, "{path}: save → reload is not stable");
    }
}

#[test]
fn an_edit_verb_carries_layout_it_does_not_touch() {
    const DOC: &str = r#"{
        "format_version": 3,
        "instrument": "t",
        "interface": {
            "inputs": { "freq": { "type": "f32", "default": 220.0, "layout": { "x": 1.5, "y": 2.5 } } },
            "outputs": { "audio": { "from": "/osc.audio", "layout": { "x": 300.0, "y": 2.5 } } }
        },
        "nodes": [
            { "type": "oscillator", "address": "/osc", "inputs": { "freq": { "from": "/freq" } },
              "layout": { "x": 150.25, "y": -40.0 } }
        ]
    }"#;
    let registry = Registry::builtin();
    let mut mem = MemoryResolver::new();
    mem.insert_text("t.json", DOC);

    edit::set_instrument_description("t.json", Some("a note"), &registry, &mem).expect("edit");
    edit::rename_instrument_node("t.json", "/osc", "/tone", &registry, &mem).expect("rename");

    let saved = mem.resolve_text("t.json").expect("saved");
    assert_eq!(
        layouts(&saved),
        vec![
            ("/tone".to_string(), (150.25, -40.0)),
            ("inputs.freq".to_string(), (1.5, 2.5)),
            ("outputs.audio".to_string(), (300.0, 2.5)),
        ]
    );
}

#[test]
fn a_malformed_layout_is_a_pointed_parse_error() {
    let registry = Registry::builtin();
    for (layout, why) in [
        (r#"{ "x": 1.0 }"#, "missing field `y`"),
        (r#"{ "x": 1.0, "y": 2.0, "z": 3.0 }"#, "unknown field `z`"),
    ] {
        let json = format!(
            r#"{{"format_version":3,"instrument":"t","nodes":[
                {{"type":"oscillator","address":"/osc","layout":{layout}}}]}}"#
        );
        let err = NormalizedDoc::from_json(&json, &registry, None)
            .expect_err("a malformed layout must not load")
            .to_string();
        assert!(err.contains(why), "expected `{why}`, got: {err}");
    }
}

/// Every `describe_instrument` view, on the projection golden's corpus (voices nested two levels
/// deep), is byte-identical with and without `layout` — the agent's grounding cost cannot move.
#[test]
fn no_projection_view_shows_layout() {
    const CORPUS: &str = "crates/reuben-document/tests/fixtures/projection/corpus";
    let registry = Registry::builtin();
    let top = Dir(CORPUS)
        .resolve_text("acid-techno.json")
        .expect("the frozen fixture instrument");
    let views = |json: &str, resolver: &dyn ResourceResolver| -> Vec<String> {
        let p = Projector::new(json, &registry, resolver).expect("the fixture mints");
        vec![
            p.index().render(),
            p.zoom(&Selection::All).render(),
            p.pipes(&Selection::All).render(),
            p.resources().render(),
        ]
    };
    assert_eq!(
        views(&top, &Dir(CORPUS)),
        views(&with_layout(&top), &Laid(Dir(CORPUS)))
    );
}
