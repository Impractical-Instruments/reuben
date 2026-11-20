//! The `call` request: one roster verb run against the engine's store, under the Coordinator lock.
//! A read answers directly; an edit is written and swapped in only if the whole open instrument
//! still loads and plans with it.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use reuben_core::Registry;
use reuben_document::coordinator::Coordinator;
use reuben_document::format::load_instrument;
use serde_json::Value;

use crate::authoring::{
    self, edit_summary, hash_instrument, CallKind, Called, Diag, EditResult, Report,
};
use crate::resources::{Adapter, ResolveError, Resources, SampleBuffer};

use super::super::wire::{Response, SwapReport};
use super::{prepare_recording, went_live, ServerStore, StructureState};

/// The canonical sources the document at `origin` reaches, itself included: one load through
/// `store`, building no Plan. A document that does not load still names the sources it read.
pub(super) fn read_tree(store: &ServerStore, origin: &str) -> BTreeSet<String> {
    let log = store.inner().record_reads();
    if let Ok(text) = store.read_text(origin) {
        let anchored = Anchored {
            store,
            referrer: store.canonical(origin, None),
        };
        let _ = load_instrument(&text, &Registry::builtin(), &Adapter(&anchored));
    }
    let mut tree = log.finish();
    tree.insert(store.canonical(origin, None));
    tree
}

/// `store` with the top document's references resolved from `referrer`, as the Coordinator's
/// `prepare_document_from` resolves them, so a read pass and a prepare record the same sources.
struct Anchored<'a> {
    store: &'a dyn Resources,
    referrer: String,
}

impl Resources for Anchored<'_> {
    fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
        self.store.read_samples(source)
    }

    fn read_text(&self, source: &str) -> Result<String, ResolveError> {
        self.store.read_text(source)
    }

    fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
        self.store
            .canonical(source, referrer.or(Some(&self.referrer)))
    }
}

/// One roster verb, under the Coordinator lock for its whole run — reads included, so no read
/// sees another call's write still waiting in memory, and every path below leaves nothing
/// pending.
///
/// - A read answers directly.
/// - A create writes at once: it never overwrites, so it cannot break what plays, and nothing
///   is swapped.
/// - An edit goes live or is refused whole: see [`edit_or_refuse`].
/// - A verb that refuses to run answers [`Response::Refused`].
///
/// [`DocumentHistory`] has already recorded an edit refused here as written. That is harmless: a
/// later restore to its hash passes through this same check.
pub(super) fn handle_call(state: &StructureState, tool: &str, args: Value) -> Response {
    let mut coordinator = match state.coordinator() {
        Ok(coordinator) => coordinator,
        Err(poisoned) => return poisoned.into(),
    };
    let staged = state.store.inner();
    let Called {
        output,
        summary,
        kind,
    } = match authoring::call(tool, args, &*state.store) {
        Ok(called) => called,
        Err(refusal) => {
            staged.discard();
            return Response::Refused {
                message: refusal.message,
            };
        }
    };
    match kind {
        CallKind::Read => {
            staged.discard();
            Response::Called { output, summary }
        }
        CallKind::Create { source } => match staged.commit() {
            Ok(()) => Response::Called { output, summary },
            Err(e) => not_written(state, &source, vec![diag(e.to_string())], Vec::new()),
        },
        CallKind::Edit { source } => {
            edit_or_refuse(state, &mut coordinator, output, summary, &source)
        }
    }
}

/// An edit the verb has staged: write it and swap the new Plan in only if the open instrument,
/// with the edit, still loads and plans; otherwise discard it and say why.
fn edit_or_refuse(
    state: &StructureState,
    coordinator: &mut Coordinator,
    output: Value,
    summary: String,
    source: &str,
) -> Response {
    let staged = state.store.inner();
    let mut result: EditResult = match serde_json::from_value(output) {
        Ok(result) => result,
        Err(e) => {
            staged.discard();
            return Response::Error {
                message: format!("an edit verb answered something other than an edit: {e}"),
            };
        }
    };
    if !result.written {
        // The verb refused the edit itself (its own validation or `expect` guard): nothing is
        // staged, and its errors say more than any refusal below would.
        staged.discard();
        return Response::Called {
            output: edit_output(&result),
            summary,
        };
    }

    let top = match state.installed_source() {
        Ok(top) => top.clone(),
        Err(poisoned) => {
            staged.discard();
            return poisoned.into();
        }
    };
    let Some(top) = top else {
        return not_written(
            state,
            source,
            vec![diag(
                "no instrument document is open; swap one in by path to edit it".to_string(),
            )],
            Vec::new(),
        );
    };
    let in_tree = match state.tree_cell() {
        Ok(tree) => tree.contains(&state.store.canonical(source, None)),
        Err(poisoned) => {
            staged.discard();
            return poisoned.into();
        }
    };
    if !in_tree {
        return not_written(
            state,
            source,
            vec![diag(format!(
                "{source} is not part of the open instrument; open it to edit it"
            ))],
            Vec::new(),
        );
    }
    let origin = state.host.document_origin(&top);
    // As a by-path swap does, so the build peaks at live + new.
    drop(coordinator.try_reclaim());
    let unloadable =
        format!("the edit to {source} leaves {top} unable to load; nothing was written");
    let text = match state.store.read_text(&origin) {
        Ok(text) => text,
        Err(e) => {
            return not_written(
                state,
                source,
                vec![diag(format!("{top} could not be read: {e}"))],
                vec![unloadable],
            )
        }
    };
    let (prepared, tree) = prepare_recording(state, coordinator, &text, &origin);
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(rejected) => {
            return not_written(
                state,
                source,
                SwapReport::from_core(rejected).report.errors,
                vec![unloadable],
            );
        }
    };
    if coordinator.swap_in_flight() {
        drop(prepared);
        return not_written(
            state,
            source,
            vec![diag(
                "a previous swap is still in flight: the engine has not finished installing it"
                    .to_string(),
            )],
            vec![format!(
                "nothing was written and {top} keeps playing; try the edit again"
            )],
        );
    }
    if let Err(e) = staged.commit() {
        drop(prepared);
        return not_written(
            state,
            source,
            vec![diag(e.to_string())],
            vec![format!(
                "{source} could not be written; nothing was written and {top} keeps playing"
            )],
        );
    }

    // Unreachable refusal: nothing was in flight under this same lock, and the swap was prepared
    // by this Coordinator at its own config. A panic here poisons the lock rather than answer for
    // a file that no longer matches what plays.
    let swapped = coordinator.commit_swap(prepared);
    assert!(
        swapped.report.ok,
        "commit_swap refused with no swap in flight: {:?}",
        swapped.report.errors
    );
    match went_live(state, coordinator, Some(top), tree) {
        Ok(warnings) if warnings.is_empty() => Response::Called {
            output: edit_output(&result),
            summary,
        },
        Ok(warnings) => {
            result.report.warnings.extend(warnings);
            Response::Called {
                output: edit_output(&result),
                summary: edit_summary(&result),
            }
        }
        Err(poisoned) => poisoned.into(),
    }
}

/// An edit that was not written: drop what is pending and answer with what `source` still holds.
fn not_written(
    state: &StructureState,
    source: &str,
    errors: Vec<Diag>,
    notes: Vec<String>,
) -> Response {
    state.store.inner().discard();
    let hash = state
        .store
        .read_text(source)
        .ok()
        .and_then(|text| hash_instrument(&text, &*state.store).ok())
        .unwrap_or_default();
    let result = EditResult {
        report: Report {
            ok: false,
            errors,
            warnings: Vec::new(),
        },
        pre_existing: Vec::new(),
        written: false,
        hash,
        notes,
        zoom: String::new(),
    };
    Response::Called {
        output: edit_output(&result),
        summary: edit_summary(&result),
    }
}

fn edit_output(result: &EditResult) -> Value {
    serde_json::to_value(result).expect("an edit result serializes to JSON")
}

fn diag(message: String) -> Diag {
    Diag {
        node: None,
        port: None,
        message,
    }
}

#[cfg(all(test, feature = "render"))]
mod tests {
    use alloc::boxed::Box;
    use alloc::collections::BTreeMap;
    use alloc::sync::Arc;
    use core::cell::RefCell;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::authoring::DocumentHistory;
    use crate::engine::server::{dispatch, EngineHost, IngressClosed, Installed};
    use crate::engine::wire::{ControlMessage, DiagnosticsReport, Request};
    use crate::render::RenderSlot;
    use crate::staged::Staged;

    /// Gives up reclaiming at once, so a swap whose retiree the test has not rendered home stays
    /// in flight until a later reclaim finds it. Holds every client swap when `hold` is set.
    struct Host {
        hold: bool,
    }

    impl EngineHost for Host {
        fn read_document(&self, path: &str) -> Result<String, String> {
            Err(format!("no file {path}"))
        }
        fn hold_swap(&self, _path: &str) -> bool {
            self.hold
        }
        fn deliver_control(&self, _: Vec<ControlMessage>) -> Result<(), IngressClosed> {
            Ok(())
        }
        fn diagnostics(&self) -> DiagnosticsReport {
            DiagnosticsReport::default()
        }
        fn publish_render_config(&self, _: usize, _: usize) -> Vec<Diag> {
            Vec::new()
        }
        fn retire_poll(&self) -> Box<dyn FnMut() -> bool + Send + '_> {
            Box::new(|| true)
        }
    }

    /// An in-memory store whose texts a test reads back to see what was written.
    #[derive(Default)]
    struct Files(Mutex<BTreeMap<String, String>>);

    impl Files {
        fn get(&self, source: &str) -> Option<String> {
            self.0.lock().unwrap().get(source).cloned()
        }
    }

    impl Resources for Files {
        fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            Err(ResolveError::NotFound(source.to_string()))
        }
        fn read_text(&self, source: &str) -> Result<String, ResolveError> {
            self.get(source)
                .ok_or_else(|| ResolveError::NotFound(source.to_string()))
        }
        fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
            self.0
                .lock()
                .unwrap()
                .insert(source.to_string(), text.to_string());
            Ok(())
        }
    }

    /// The open instrument: its `/voice` node nests `v.json` and wires out what `v.json` names `out`.
    const TOP: &str = r#"{"format_version":3,"instrument":"top",
        "resources":{"v":"v.json"},
        "interface":{"outputs":{"out":{"from":"/voice.out"}}},
        "nodes":[{"type":"subpatch","address":"/voice","patch":"v"}]}"#;

    const V: &str = r#"{"format_version":3,"instrument":"v",
        "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

    /// Valid on its own and reached by nothing `top.json` loads.
    const OTHER: &str = r#"{"format_version":3,"instrument":"other",
        "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[{"type":"oscillator","address":"/osc"}]}"#;

    /// A state serving `top.json` from an in-memory store, through the one `Staged` the Coordinator
    /// also resolves through, and the render side it swaps into.
    struct Served {
        state: StructureState,
        files: Arc<Files>,
        slot: RefCell<RenderSlot>,
    }

    impl Served {
        /// Render blocks until the render side has installed any pending swap and posted its
        /// retiree, as a running callback would between two requests.
        fn render(&self) {
            let mut slot = self.slot.borrow_mut();
            let mut buf = vec![0.0f32; 128 * slot.channels().max(1)];
            for _ in 0..BLOCKS_BETWEEN_CALLS {
                slot.fill(&mut buf);
            }
        }
    }

    /// Enough blocks for the render side to finish a swap, its declick ramp included.
    const BLOCKS_BETWEEN_CALLS: usize = 64;

    fn serving() -> Served {
        serving_with(Host { hold: false })
    }

    fn serving_with(host: Host) -> Served {
        let files = Arc::new(Files::default());
        for (source, text) in [("top.json", TOP), ("v.json", V), ("other.json", OTHER)] {
            files.write_text(source, text).unwrap();
        }
        let staged: Staged<Box<dyn Resources + Send + Sync>> =
            Staged::new(Box::new(Arc::clone(&files)));
        let (coordinator, side, _) = crate::engine::install_initial(
            TOP,
            staged.clone(),
            reuben_core::AudioConfig::new(48e3, 128),
        )
        .expect("install");
        let state = StructureState::new(
            coordinator,
            Arc::new(host),
            Arc::new(DocumentHistory::new(staged)),
        )
        .with_installed_source(Some("top.json".to_string()));
        Served {
            state,
            files,
            slot: RefCell::new(RenderSlot::new(side)),
        }
    }

    /// Run `tool` and then let the render side catch up, as it would before the next request.
    fn call(served: &Served, tool: &str, args: Value) -> (Value, String) {
        let answer = call_undrained(&served.state, tool, args);
        served.render();
        answer
    }

    /// Run `tool` and render nothing after it, so a swap it made stays in flight.
    fn call_undrained(state: &StructureState, tool: &str, args: Value) -> (Value, String) {
        let line = Request::Call {
            tool: tool.to_string(),
            args,
        }
        .to_ndjson();
        match dispatch(state, &line) {
            Response::Called { output, summary } => (output, summary),
            other => panic!("expected a called answer, got {other:?}"),
        }
    }

    fn installed_hash(state: &StructureState) -> String {
        match state.try_installed() {
            Installed::Known { content_hash, .. } => content_hash,
            other => panic!("{other:?}"),
        }
    }

    fn open_line(name: &str, expect: Option<String>) -> String {
        Request::Open {
            name: name.to_string(),
            expect,
        }
        .to_ndjson()
    }

    #[test]
    fn open_installs_and_records_tree() {
        let s = serving();
        match dispatch(&s.state, &open_line("other.json", None)) {
            Response::SwapReport(report) => assert!(report.report.ok, "{:?}", report.report.errors),
            other => panic!("expected a swap report, got {other:?}"),
        }
        s.render();
        assert_eq!(s.state.tree(), BTreeSet::from(["other.json".to_string()]));
        match s.state.try_installed() {
            Installed::Known { source, .. } => assert_eq!(source.as_deref(), Some("other.json")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn open_of_an_unreadable_name_is_rejected_and_keeps_the_tree() {
        let s = serving();
        let before = installed_hash(&s.state);
        match dispatch(&s.state, &open_line("missing.json", None)) {
            Response::SwapReport(report) => {
                assert!(!report.report.ok);
                assert_eq!(report.content_hash, before);
            }
            other => panic!("expected a swap report, got {other:?}"),
        }
        assert_eq!(
            s.state.tree(),
            BTreeSet::from(["top.json".to_string(), "v.json".to_string()])
        );
    }

    #[test]
    fn open_is_held_for_a_holding_host() {
        let s = serving_with(Host { hold: true });
        let before = installed_hash(&s.state);
        match dispatch(&s.state, &open_line("other.json", None)) {
            Response::Held(held) => assert_eq!(held.requested, "other.json"),
            other => panic!("expected a held open, got {other:?}"),
        }
        assert_eq!(installed_hash(&s.state), before);
    }

    #[test]
    fn open_with_stale_expect_conflicts() {
        let s = serving();
        match dispatch(&s.state, &open_line("other.json", Some("0badc0de".into()))) {
            Response::Conflict(conflict) => assert_eq!(conflict.expected, "0badc0de"),
            other => panic!("expected a conflict, got {other:?}"),
        }
    }

    #[test]
    fn list_asks_the_host_with_the_open_source() {
        let s = serving();
        let line = Request::List {
            what: crate::engine::ListWhat::Samples,
        }
        .to_ndjson();
        assert_eq!(
            dispatch(&s.state, &line),
            Response::Listed { names: Vec::new() }
        );
        assert!(matches!(
            Response::from_ndjson(r#"{"reply":"listed","names":["a"]}"#),
            Ok(Response::Listed { .. })
        ));
        assert!(Request::from_ndjson(r#"{"verb":"list","what":"nonsense"}"#).is_err());
    }

    #[test]
    fn the_initial_install_records_the_open_tree() {
        let s = serving();
        let state = &s.state;
        assert_eq!(
            state.tree(),
            BTreeSet::from(["top.json".to_string(), "v.json".to_string()])
        );
    }

    #[test]
    fn read_call_answers_without_swapping() {
        let s = serving();
        let state = &s.state;
        let before = installed_hash(state);
        let (output, _) = call(&s, "describe_instrument", json!({"source": "top.json"}));
        assert!(!output.is_null());
        assert_eq!(installed_hash(state), before);
    }

    #[test]
    fn edit_call_goes_live() {
        let s = serving();
        let (state, files) = (&s.state, &s.files);
        let (output, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "top.json", "name": "renamed"}),
        );
        assert_eq!(output["written"], true, "{output}");
        assert_eq!(json!(installed_hash(state)), output["hash"]);
        assert!(files.get("top.json").unwrap().contains("renamed"));
        assert_eq!(
            state.tree(),
            BTreeSet::from(["top.json".to_string(), "v.json".to_string()])
        );
    }

    #[test]
    fn edit_that_breaks_parent_is_refused_and_writes_nothing() {
        let s = serving();
        let (state, files) = (&s.state, &s.files);
        let before = installed_hash(state);
        let (output, _) = call(
            &s,
            "remove_instrument_interface_output",
            json!({"source": "v.json", "name": "out"}),
        );
        assert_eq!(output["written"], false, "{output}");
        assert!(
            output["notes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n.as_str().unwrap().contains("nothing was written")),
            "{output}"
        );
        assert_eq!(files.get("v.json").unwrap(), V);
        assert_eq!(installed_hash(state), before);
    }

    #[test]
    fn edit_outside_tree_is_refused() {
        let s = serving();
        let files = &s.files;
        let (output, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "other.json", "name": "renamed"}),
        );
        assert_eq!(output["written"], false, "{output}");
        assert!(
            output["report"]["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("not part of the open instrument"),
            "{output}"
        );
        assert_eq!(files.get("other.json").unwrap(), OTHER);
    }

    #[test]
    fn an_edit_outside_the_tree_the_verb_refused_reports_the_verbs_errors() {
        let s = serving();
        let (output, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "other.json", "name": "x", "expect": "0badc0de"}),
        );
        assert_eq!(output["written"], false, "{output}");
        let message = output["report"]["errors"][0]["message"].as_str().unwrap();
        assert!(message.contains("0badc0de"), "{output}");
        assert!(
            !message.contains("not part of the open instrument"),
            "{output}"
        );
    }

    #[test]
    fn stale_expect_on_call_edit_is_refused() {
        let s = serving();
        let state = &s.state;
        let stale = installed_hash(state);
        let (first, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "top.json", "name": "one", "expect": stale}),
        );
        assert_eq!(first["written"], true, "{first}");
        let (second, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "top.json", "name": "two", "expect": stale}),
        );
        assert_eq!(second["written"], false, "{second}");
        // The verb's own guard refused it, naming the hash it was wrongly told to expect — not
        // the engine, which would have answered with a note.
        let message = second["report"]["errors"][0]["message"].as_str().unwrap();
        assert!(message.contains(&stale), "{second}");
        assert!(second.get("notes").is_none(), "{second}");
        assert_eq!(json!(installed_hash(state)), first["hash"]);
    }

    #[test]
    fn two_successive_edits_both_go_live() {
        let s = serving();
        let (state, files) = (&s.state, &s.files);
        let (first, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "top.json", "name": "one"}),
        );
        assert_eq!(first["written"], true, "{first}");
        let (second, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "top.json", "name": "two", "expect": first["hash"]}),
        );
        assert_eq!(second["written"], true, "{second}");
        assert_ne!(second["hash"], first["hash"]);
        assert_eq!(json!(installed_hash(state)), second["hash"]);
        assert!(files.get("top.json").unwrap().contains("two"));
    }

    #[test]
    fn an_edit_while_a_swap_is_in_flight_writes_nothing() {
        // Nothing is rendered between the two, and the host gives up reclaiming at once, so the
        // first edit's swap never comes home and stays in flight.
        let s = serving();
        let (state, files) = (&s.state, &s.files);
        let (first, _) = call_undrained(
            state,
            "set_instrument_name",
            json!({"source": "top.json", "name": "one"}),
        );
        assert_eq!(first["written"], true, "{first}");
        let written = files.get("top.json").unwrap();

        let (second, _) = call_undrained(
            state,
            "set_instrument_name",
            json!({"source": "top.json", "name": "two"}),
        );
        assert_eq!(second["written"], false, "{second}");
        assert_eq!(second["report"]["ok"], false, "{second}");
        assert!(
            second["report"]["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("still in flight"),
            "{second}"
        );
        assert!(
            second["notes"][0]
                .as_str()
                .unwrap()
                .contains("nothing was written"),
            "{second}"
        );
        assert_eq!(second["hash"], first["hash"]);
        assert_eq!(files.get("top.json").unwrap(), written);
        assert_eq!(json!(installed_hash(state)), first["hash"]);
    }

    #[test]
    fn create_call_writes_without_swapping() {
        let s = serving();
        let (state, files) = (&s.state, &s.files);
        let before = installed_hash(state);
        let (output, _) = call(
            &s,
            "new_instrument",
            json!({"source": "n.json", "name": "n"}),
        );
        assert_eq!(output["written"], true, "{output}");
        assert!(files.get("n.json").is_some());
        assert_eq!(installed_hash(state), before);
    }

    #[test]
    fn a_verb_refusal_is_refused() {
        let s = serving();
        let state = &s.state;
        let line = Request::Call {
            tool: "no_such_verb".to_string(),
            args: json!({}),
        }
        .to_ndjson();
        match dispatch(state, &line) {
            Response::Refused { message } => assert!(message.contains("unknown tool"), "{message}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn an_edit_with_no_open_document_is_refused() {
        let mut s = serving();
        s.state = s.state.clone().with_installed_source(None);
        let (output, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "top.json", "name": "x"}),
        );
        assert_eq!(output["written"], false, "{output}");
        assert!(
            output["report"]["errors"][0]["message"]
                .as_str()
                .unwrap()
                .contains("no instrument document is open"),
            "{output}"
        );
    }
}
