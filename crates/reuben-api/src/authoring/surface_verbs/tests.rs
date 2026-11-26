use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use std::cell::RefCell;

use serde_json::{json, Value};

use crate::authoring::*;
use crate::surface::{surface_hash, SurfaceDoc, SurfaceKind};
use crate::tools::names;

/// Sources are exact keys. An instrument `<stem>.json` keeps its surface docs at
/// `surfaces/<stem><suffix>.json`, unless the store is built to keep none.
struct MemoryStore {
    texts: RefCell<BTreeMap<String, String>>,
    keeps_surfaces: bool,
}

impl Resources for MemoryStore {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        Err(ResolveError::NotFound(source.to_string()))
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        self.texts
            .borrow()
            .get(source)
            .cloned()
            .ok_or_else(|| ResolveError::NotFound(source.to_string()))
    }

    fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
        self.texts
            .borrow_mut()
            .insert(source.to_string(), text.to_string());
        Ok(())
    }

    fn surface_source(&self, instrument: &str, surface: SurfaceKind) -> Option<String> {
        let stem = instrument.strip_suffix(".json")?;
        self.keeps_surfaces
            .then(|| format!("surfaces/{stem}{}.json", surface.suffix()))
    }
}

const SYNTH: &str = r#"{
    "format_version": 4,
    "instrument": "synth",
    "interface": { "inputs": {
        "cutoff": { "type": "f32", "default": 1000.0, "min": 20.0, "max": 20000.0, "curve": "exp" },
        "notes": { "type": "note" },
        "mode": { "type": "FilterMode", "default": "Lp" }
    } },
    "nodes": [ { "type": "oscillator", "address": "/osc" } ]
}"#;

const MAP: &str = "surfaces/synth.midi.json";
const SCREEN: &str = "surfaces/synth.json";

fn store() -> DocumentHistory<MemoryStore> {
    let store = MemoryStore {
        texts: RefCell::new(BTreeMap::new()),
        keeps_surfaces: true,
    };
    store.write_text("synth.json", SYNTH).unwrap();
    DocumentHistory::new(store)
}

fn with_map(controls: Value) -> DocumentHistory<MemoryStore> {
    let store = store();
    put_map(&store, "synth", controls);
    store
}

fn put_map(store: &DocumentHistory<MemoryStore>, instrument: &str, controls: Value) {
    let text = json!({"surface_version": 1, "instrument": instrument, "controls": controls});
    store.inner().write_text(MAP, &text.to_string()).unwrap();
}

fn text(store: &DocumentHistory<MemoryStore>, source: &str) -> Option<String> {
    store.inner().texts.borrow().get(source).cloned()
}

fn map(store: &DocumentHistory<MemoryStore>) -> SurfaceDoc {
    SurfaceDoc::parse(&text(store, MAP).expect("a map")).expect("a map that parses")
}

fn binds(doc: &SurfaceDoc) -> Vec<&str> {
    doc.controls.iter().map(|c| c.bind.as_str()).collect()
}

fn edit(store: &DocumentHistory<MemoryStore>, tool: &str, args: Value) -> EditResult {
    let called = call(tool, args, store).unwrap_or_else(|r| panic!("{tool}: {r}"));
    serde_json::from_value(called.output).expect("an edit result")
}

fn refused(store: &DocumentHistory<MemoryStore>, tool: &str, args: Value) -> String {
    call(tool, args, store).expect_err("a refusal").message
}

fn describe(store: &DocumentHistory<MemoryStore>, surface: &str) -> SurfaceView {
    let called = call(
        names::DESCRIBE_INSTRUMENT_SURFACE,
        json!({"source": "synth.json", "surface": surface}),
        store,
    )
    .expect("a read");
    assert_eq!(called.kind, CallKind::Read);
    serde_json::from_value(called.output).expect("a surface view")
}

fn set(control: Value, index: Option<usize>) -> Value {
    let mut args = json!({"source": "synth.json", "surface": "midi", "control": control});
    if let Some(index) = index {
        args["index"] = json!(index);
    }
    args
}

#[test]
fn set_appends_a_midi_control_and_creates_the_map() {
    let store = store();
    let called = call(
        names::SET_INSTRUMENT_SURFACE_CONTROL,
        set(
            json!({"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}}),
            None,
        ),
        &store,
    )
    .expect("an edit");
    assert_eq!(
        called.kind,
        CallKind::Edit {
            source: MAP.to_string()
        }
    );
    let result: EditResult = serde_json::from_value(called.output).unwrap();
    assert!(result.written, "{result:?}");
    assert!(result.report.ok);
    let written = map(&store);
    assert_eq!(written.instrument, "synth");
    assert_eq!(binds(&written), ["cutoff"]);
    assert_eq!(result.hash, surface_hash(&written));
    assert_eq!(text(&store, MAP).unwrap(), written.to_file_text());
    assert_eq!(result.zoom, "0 cutoff \u{2190} cc 74 on channel 1");
}

#[test]
fn set_replaces_at_index() {
    let store = with_map(json!([
        {"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}},
        {"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 1}}
    ]));
    let result = edit(
        &store,
        names::SET_INSTRUMENT_SURFACE_CONTROL,
        set(
            json!({"bind": "notes", "midi": {"kind": "note", "channel": 1}}),
            Some(1),
        ),
    );
    assert!(result.written, "{result:?}");
    let written = map(&store);
    assert_eq!(binds(&written), ["cutoff", "notes"]);
    assert_eq!(
        written.controls[1]
            .midi
            .as_ref()
            .map(ToString::to_string)
            .as_deref(),
        Some("note on channel 1")
    );
}

#[test]
fn set_refuses_a_control_that_would_not_route() {
    let store =
        with_map(json!([{"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}}]));
    let before = text(&store, MAP);
    let result = edit(
        &store,
        names::SET_INSTRUMENT_SURFACE_CONTROL,
        set(
            json!({"bind": "mode", "midi": {"kind": "cc", "channel": 1, "cc": 74}}),
            None,
        ),
    );
    assert!(!result.written);
    assert!(!result.report.ok);
    assert_eq!(result.hash, surface_hash(&map(&store)));
    let error = &result.report.errors[0];
    assert_eq!(error.port.as_deref(), Some("mode"));
    assert!(
        error.message.contains("\"mode\"") && error.message.contains("enum"),
        "{error:?}"
    );
    assert_eq!(text(&store, MAP), before);
}

#[test]
fn a_stale_expect_writes_nothing() {
    let store = with_map(json!([
        {"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}},
        {"bind": "notes", "midi": {"kind": "note", "channel": 1}}
    ]));
    let before = text(&store, MAP);
    let current = surface_hash(&map(&store));
    for (tool, mut args) in [
        (
            names::SET_INSTRUMENT_SURFACE_CONTROL,
            set(
                json!({"bind": "cutoff", "midi": {"kind": "cc", "channel": 2, "cc": 74}}),
                None,
            ),
        ),
        (
            names::REMOVE_INSTRUMENT_SURFACE_CONTROL,
            json!({"source": "synth.json", "surface": "midi", "index": 0}),
        ),
        (
            names::MOVE_INSTRUMENT_SURFACE_CONTROL,
            json!({"source": "synth.json", "surface": "midi", "from": 0, "to": 1}),
        ),
    ] {
        args["expect"] = json!("0000000000000000");
        let result = edit(&store, tool, args);
        assert!(!result.written, "{tool}");
        assert_eq!(result.hash, current, "{tool}");
        assert!(
            result.report.errors[0].message.contains("expect guard"),
            "{tool}: {result:?}"
        );
        assert_eq!(text(&store, MAP), before, "{tool}");
    }
    let mut args = set(
        json!({"bind": "cutoff", "midi": {"kind": "cc", "channel": 2, "cc": 74}}),
        None,
    );
    args["expect"] = json!(current);
    assert!(edit(&store, names::SET_INSTRUMENT_SURFACE_CONTROL, args).written);
}

#[test]
fn an_expect_on_a_missing_doc_says_it_does_not_exist() {
    let store = store();
    let mut args = set(
        json!({"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}}),
        None,
    );
    args["expect"] = json!("0000000000000000");
    let result = edit(&store, names::SET_INSTRUMENT_SURFACE_CONTROL, args);
    assert!(!result.written);
    assert_eq!(result.hash, "");
    let message = &result.report.errors[0].message;
    assert!(
        message.contains("the MIDI map does not exist")
            && message.contains("not the expected 0000000000000000"),
        "{message}"
    );
    assert_eq!(text(&store, MAP), None);
    // An absent doc's hash is empty, so `""` expects the doc to be absent.
    let mut args = set(
        json!({"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}}),
        None,
    );
    args["expect"] = json!("");
    assert!(edit(&store, names::SET_INSTRUMENT_SURFACE_CONTROL, args).written);
}

#[test]
fn a_map_that_does_not_parse_is_never_overwritten() {
    for (broken, says) in [
        ("{", "is not a surface doc"),
        (
            r#"{"surface_version":2,"instrument":"synth","controls":[]}"#,
            "surface_version 2",
        ),
    ] {
        let store = store();
        store.inner().write_text(MAP, broken).unwrap();
        for (tool, args) in [
            (
                names::SET_INSTRUMENT_SURFACE_CONTROL,
                set(
                    json!({"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}}),
                    None,
                ),
            ),
            (
                names::REMOVE_INSTRUMENT_SURFACE_CONTROL,
                json!({"source": "synth.json", "surface": "midi", "index": 0}),
            ),
            (
                names::MOVE_INSTRUMENT_SURFACE_CONTROL,
                json!({"source": "synth.json", "surface": "midi", "from": 0, "to": 0}),
            ),
        ] {
            let result = edit(&store, tool, args);
            assert!(!result.written, "{tool}");
            assert_eq!(result.hash, "", "{tool}");
            assert!(
                result.report.errors[0].message.contains(says),
                "{tool}: {result:?}"
            );
            assert_eq!(text(&store, MAP).as_deref(), Some(broken), "{tool}");
        }
        let view = describe(&store, "midi");
        assert!(view.exists);
        assert!(
            view.error.as_deref().is_some_and(|e| e.contains(says)),
            "{view:?}"
        );
        assert!(view.controls.is_empty());
        assert_eq!(view.hash, None);
    }
}

#[test]
fn remove_and_move_reorder_and_out_of_range_is_a_refusal() {
    let store = with_map(json!([
        {"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}},
        {"bind": "notes", "midi": {"kind": "note", "channel": 1}},
        {"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 1}}
    ]));
    let moved = edit(
        &store,
        names::MOVE_INSTRUMENT_SURFACE_CONTROL,
        json!({"source": "synth.json", "surface": "midi", "from": 0, "to": 2}),
    );
    assert!(moved.written, "{moved:?}");
    let written = map(&store);
    assert_eq!(
        written
            .controls
            .iter()
            .map(|c| c.midi.as_ref().unwrap().to_string())
            .collect::<Vec<_>>(),
        [
            "note on channel 1",
            "cc 1 on channel 1",
            "cc 74 on channel 1"
        ]
    );
    assert_eq!(moved.hash, surface_hash(&written));

    let removed = edit(
        &store,
        names::REMOVE_INSTRUMENT_SURFACE_CONTROL,
        json!({"source": "synth.json", "surface": "midi", "index": 0}),
    );
    assert!(removed.written);
    assert_eq!(binds(&map(&store)), ["cutoff", "cutoff"]);

    let before = text(&store, MAP);
    for (tool, args) in [
        (
            names::REMOVE_INSTRUMENT_SURFACE_CONTROL,
            json!({"source": "synth.json", "surface": "midi", "index": 5}),
        ),
        (
            names::MOVE_INSTRUMENT_SURFACE_CONTROL,
            json!({"source": "synth.json", "surface": "midi", "from": 5, "to": 0}),
        ),
        (
            names::MOVE_INSTRUMENT_SURFACE_CONTROL,
            json!({"source": "synth.json", "surface": "midi", "from": 0, "to": 5}),
        ),
        (
            names::SET_INSTRUMENT_SURFACE_CONTROL,
            set(
                json!({"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 7}}),
                Some(5),
            ),
        ),
    ] {
        let why = refused(&store, tool, args);
        assert!(why.contains('5') && why.contains('2'), "{tool}: {why}");
    }
    assert_eq!(text(&store, MAP), before);

    for _ in 0..2 {
        edit(
            &store,
            names::REMOVE_INSTRUMENT_SURFACE_CONTROL,
            json!({"source": "synth.json", "surface": "midi", "index": 0}),
        );
    }
    assert!(map(&store).controls.is_empty(), "the file stays, empty");
}

#[test]
fn remove_and_move_on_a_missing_doc_are_refused() {
    let store = store();
    for (tool, args, says) in [
        (
            names::REMOVE_INSTRUMENT_SURFACE_CONTROL,
            json!({"source": "synth.json", "surface": "midi", "index": 0}),
            "has no MIDI map",
        ),
        (
            names::MOVE_INSTRUMENT_SURFACE_CONTROL,
            json!({"source": "synth.json", "surface": "screen", "from": 0, "to": 0}),
            "has no surface doc",
        ),
    ] {
        let why = refused(&store, tool, args);
        assert!(why.contains(says), "{tool}: {why}");
    }
    assert_eq!(text(&store, MAP), None);
}

#[test]
fn describe_reports_per_control_status_and_doc_warnings() {
    let store = store();
    put_map(
        &store,
        "other",
        json!([
            {"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}, "min": 10},
            {"bind": "mode", "midi": {"kind": "cc", "channel": 1, "cc": 1}}
        ]),
    );
    let view = describe(&store, "midi");
    assert!(view.exists);
    assert_eq!(view.source, MAP);
    assert_eq!(view.instrument.as_deref(), Some("other"));
    assert_eq!(view.hash, Some(surface_hash(&map(&store))));
    assert_eq!(view.error, None);
    assert_eq!(view.controls.len(), 2);
    assert_eq!(view.controls[0].skipped, None);
    assert_eq!(view.controls[0].warnings.len(), 1, "{view:?}");
    assert!(view.controls[0].warnings[0].contains("clamped"));
    let skipped = view.controls[1].skipped.as_deref().expect("skipped");
    assert!(skipped.contains("\"mode\""), "{skipped}");
    assert!(
        view.warnings
            .iter()
            .any(|w| w.contains("written for instrument `other`")),
        "{view:?}"
    );

    // A written edit reports the same per-control findings, each on its bind.
    let moved = edit(
        &store,
        names::MOVE_INSTRUMENT_SURFACE_CONTROL,
        json!({"source": "synth.json", "surface": "midi", "from": 1, "to": 0}),
    );
    assert!(moved.written);
    let on = |port: Option<&str>| {
        moved
            .report
            .warnings
            .iter()
            .filter(|d| d.port.as_deref() == port)
            .count()
    };
    assert_eq!(
        (on(Some("mode")), on(Some("cutoff")), on(None)),
        (1, 1, 1),
        "{moved:?}"
    );
}

#[test]
fn describe_of_a_missing_doc_says_it_does_not_exist() {
    let view = describe(&store(), "midi");
    assert_eq!(
        view,
        SurfaceView {
            source: MAP.to_string(),
            exists: false,
            hash: None,
            instrument: None,
            cols: None,
            controls: Vec::new(),
            warnings: Vec::new(),
            error: None,
        }
    );
}

#[test]
fn screen_set_accepts_xy_pad() {
    let store = store();
    let result = edit(
        &store,
        names::SET_INSTRUMENT_SURFACE_CONTROL,
        json!({"source": "synth.json", "surface": "screen",
               "control": {"bind": "cutoff", "widget": "xy-pad", "label": "Pad"}}),
    );
    assert!(result.written, "{result:?}");
    let doc = SurfaceDoc::parse(&text(&store, SCREEN).unwrap()).unwrap();
    assert_eq!(doc.controls[0].widget.as_deref(), Some("xy-pad"));
    assert_eq!(text(&store, MAP), None);
    let view = describe(&store, "screen");
    assert_eq!(view.source, SCREEN);
    assert_eq!(view.controls[0].skipped, None);
}

#[test]
fn a_store_that_keeps_no_surface_docs_refuses_every_verb() {
    let store = DocumentHistory::new(MemoryStore {
        texts: RefCell::new(BTreeMap::new()),
        keeps_surfaces: false,
    });
    store.write_text("synth.json", SYNTH).unwrap();
    for (tool, args) in [
        (
            names::DESCRIBE_INSTRUMENT_SURFACE,
            json!({"source": "synth.json", "surface": "midi"}),
        ),
        (
            names::SET_INSTRUMENT_SURFACE_CONTROL,
            set(
                json!({"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}}),
                None,
            ),
        ),
        (
            names::REMOVE_INSTRUMENT_SURFACE_CONTROL,
            json!({"source": "synth.json", "surface": "midi", "index": 0}),
        ),
        (
            names::MOVE_INSTRUMENT_SURFACE_CONTROL,
            json!({"source": "synth.json", "surface": "midi", "from": 0, "to": 0}),
        ),
    ] {
        let why = refused(&store, tool, args);
        assert!(why.contains("keeps no surface docs"), "{tool}: {why}");
    }
}

#[test]
fn an_instrument_that_does_not_load_leaves_its_map_unchecked_and_unwritten() {
    let store =
        with_map(json!([{"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 74}}]));
    store
        .inner()
        .write_text(
            "synth.json",
            r#"{"format_version":4,"instrument":"synth","nodes":[
                {"type":"oscillator","address":"/osc","inputs":{"freq":{"from":"/gone"}}}]}"#,
        )
        .unwrap();
    let before = text(&store, MAP);
    let result = edit(
        &store,
        names::SET_INSTRUMENT_SURFACE_CONTROL,
        set(
            json!({"bind": "cutoff", "midi": {"kind": "cc", "channel": 1, "cc": 7}}),
            None,
        ),
    );
    assert!(!result.written);
    assert!(
        result.report.errors[0]
            .message
            .contains("does not load, so its MIDI map cannot be checked"),
        "{result:?}"
    );
    assert_eq!(text(&store, MAP), before);
    let view = describe(&store, "midi");
    assert_eq!(view.controls.len(), 1);
    assert!(
        view.warnings.iter().any(|w| w.contains("does not load")),
        "{view:?}"
    );
}
