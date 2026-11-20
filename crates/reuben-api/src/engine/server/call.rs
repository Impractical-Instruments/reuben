//! The `call` request: one roster verb run against the engine's store, under the Coordinator lock.
//! A read answers directly; an edit is written and swapped in only if the whole open instrument
//! still loads and plans with it, unless the open instrument did not load to begin with.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use reuben_core::Registry;
use reuben_document::coordinator::Coordinator;
use reuben_document::format::load_instrument;
use serde_json::Value;

use crate::authoring::{
    self, edit_summary, hash_instrument, CallKind, Called, Diag, EditResult, Named, Report,
};
use crate::resources::{Adapter, ResolveError, Resources, SampleBuffer};

use super::super::wire::{Response, SwapReport};
use super::{prepare_recording, went_live, Poisoned, ServerStore, StructureState, Tree, Went};

/// What the document at `origin` reaches, itself included: one load through `store`, building no
/// Plan. A document that does not load still names the sources it read.
pub(super) fn read_tree(store: &ServerStore, origin: &str) -> Tree {
    let log = store.inner().record_reads();
    if let Ok(text) = store.read_text(origin) {
        let anchored = Anchored {
            store,
            referrer: store.canonical(origin, None),
        };
        let _ = load_instrument(&text, &Registry::builtin(), &Adapter(&anchored));
    }
    Tree::new(log.finish(), store.canonical(origin, None))
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
///   is swapped. Its name must end in `.json`.
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
    let named = resolved_names(state, tool, &args);
    match unoffered_name(state, &named) {
        Ok(None) => {}
        Ok(Some(name)) => {
            return Response::Refused {
                message: super::not_offered(&name),
            }
        }
        Err(poisoned) => return poisoned.into(),
    }
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
                message: in_client_terms(refusal.message, &named),
            };
        }
    };
    match kind {
        CallKind::Read => {
            staged.discard();
            Response::Called { output, summary }
        }
        // Only an instrument file is made here: a create named otherwise would leave a document
        // where `list` offers nothing, or under a sample's name.
        CallKind::Create { source } if !source.ends_with(".json") => {
            staged.discard();
            Response::Refused {
                message: format!("`{source}` does not end in .json; nothing was written"),
            }
        }
        CallKind::Create { source } => match staged.commit() {
            Ok(()) => Response::Called { output, summary },
            Err(e) => not_written(
                state,
                &source,
                vec![diag(in_client_terms(e.to_string(), &named))],
                Vec::new(),
            ),
        },
        CallKind::Edit { source } => {
            edit_or_refuse(state, &mut coordinator, output, summary, &source)
        }
    }
}

/// Each file `tool`'s `args` name, as the client spelled it and as the store resolves it.
fn resolved_names(state: &StructureState, tool: &str, args: &Value) -> Vec<(String, String)> {
    let canonical = |name: &str, referrer: Option<&str>| state.store.canonical(name, referrer);
    authoring::named_sources(tool, args)
        .unwrap_or_default()
        .into_iter()
        .map(|named| match named {
            Named::Document(name) => {
                let resolved = canonical(&name, None);
                (name, resolved)
            }
            Named::Reference { document, name } => {
                let resolved = canonical(&name, Some(&canonical(&document, None)));
                (name, resolved)
            }
        })
        .collect()
}

/// The first of `named` that the host does not offer and the open instrument's tree does not
/// already reach, as the client spelled it. An edit's own target is checked against the tree again
/// in [`edit_or_refuse`]; this keeps a read, a create, or a reference an edit writes from naming
/// what the client may not.
fn unoffered_name(
    state: &StructureState,
    named: &[(String, String)],
) -> Result<Option<String>, Poisoned> {
    let open = state.installed_source()?.clone();
    let tree = state.tree();
    let offered =
        |canonical: &str| tree.contains(canonical) || state.host.within(canonical, open.as_deref());
    Ok(named
        .iter()
        .find(|(_, resolved)| !offered(resolved))
        .map(|(name, _)| name.clone()))
}

/// `message` with each server path in `named` put back as the client spelled it, so an answer
/// names files only in the client's own terms.
fn in_client_terms(mut message: String, named: &[(String, String)]) -> String {
    let mut by_length: Vec<&(String, String)> = named.iter().collect();
    // Longest first, so a path that begins another is not replaced inside it.
    by_length.sort_by_key(|(_, resolved)| core::cmp::Reverse(resolved.len()));
    for (name, resolved) in by_length {
        message = message.replace(resolved.as_str(), name);
    }
    message
}

/// An edit the verb has staged: write it and swap the new Plan in only if the open instrument,
/// with the edit, still loads and plans; otherwise discard it and say why.
///
/// While the open instrument does not load and nothing plays, an edit that leaves it unable to
/// load is written anyway, with [`Event::Invalid`](super::Event::Invalid) for what is still
/// wrong: that is how a broken document is repaired. The edit that makes it load starts it
/// playing.
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
                "no instrument document is open; open one with `open_instrument` to edit it"
                    .to_string(),
            )],
            Vec::new(),
        );
    };
    let in_tree = match state.tree_cell() {
        Ok(tree) => tree.sources.contains(&state.store.canonical(source, None)),
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
        Err(rejected) if state.is_silent() => {
            let errors = SwapReport::from_core(rejected).report.errors;
            return written_while_silent(state, result, source, &top, tree, errors);
        }
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
                "nothing was written and {}; try the edit again",
                still_playing(state, &top)
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
                "{source} could not be written; nothing was written and {}",
                still_playing(state, &top)
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
    match went_live(state, coordinator, Some(top), tree, Went::Edited) {
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

/// What an edit that wrote nothing leaves sounding: `top`, or nothing while it does not load.
fn still_playing(state: &StructureState, top: &str) -> String {
    if state.is_silent() {
        format!("nothing plays, since {top} does not load")
    } else {
        format!("{top} keeps playing")
    }
}

/// Write an edit to an open instrument that does not load before or after it: nothing is swapped,
/// `tree` becomes the documents an edit may touch, and `errors`, what still keeps `top` from
/// loading, are published and noted.
fn written_while_silent(
    state: &StructureState,
    mut result: EditResult,
    source: &str,
    top: &str,
    tree: Tree,
    errors: Vec<Diag>,
) -> Response {
    if let Err(e) = state.store.inner().commit() {
        return not_written(
            state,
            source,
            vec![diag(e.to_string())],
            vec![format!("{source} could not be written")],
        );
    }
    match state.tree_cell() {
        Ok(mut cell) => *cell = tree,
        Err(poisoned) => return poisoned.into(),
    }
    let still = errors
        .iter()
        .map(|e| e.message.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    result.notes.push(format!(
        "written, but {top} still does not load, so nothing plays until it does: {still}"
    ));
    state.publish_invalid(errors);
    Response::Called {
        output: edit_output(&result),
        summary: edit_summary(&result),
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

pub(super) fn diag(message: String) -> Diag {
    Diag {
        node: None,
        port: None,
        message,
    }
}

#[cfg(all(test, feature = "render"))]
mod tests {
    use alloc::boxed::Box;
    use alloc::collections::{BTreeMap, BTreeSet};
    use alloc::sync::Arc;
    use core::cell::RefCell;
    use std::sync::Mutex;

    use serde_json::json;

    use super::*;
    use crate::authoring::DocumentHistory;
    use std::sync::mpsc::TryRecvError;

    use crate::engine::server::{
        dispatch, EngineHost, InProcess, IngressClosed, Installed, SwapInFlight,
    };
    use crate::engine::wire::{
        ChangedBy, ControlMessage, DiagnosticsReport, DocSource, Event, Request,
    };
    use crate::render::RenderSlot;
    use crate::staged::Staged;

    /// Gives up reclaiming at once, so a swap whose retiree the test has not rendered home stays
    /// in flight until a later reclaim finds it. Holds every client swap when `hold` is set.
    struct Host {
        hold: bool,
        /// When set, only names starting with it are within what the host offers.
        only: Option<&'static str>,
    }

    impl EngineHost for Host {
        fn read_document(&self, path: &str) -> Result<String, String> {
            Err(format!("no file {path}"))
        }
        fn hold_swap(&self, _path: &str) -> bool {
            self.hold
        }
        fn within(&self, name: &str, _open: Option<&str>) -> bool {
            self.only.is_none_or(|prefix| name.starts_with(prefix))
        }
        /// Echoes the open source it was given, so a test sees what the server passed.
        fn offered(&self, _what: crate::engine::ListWhat, open: Option<&str>) -> Vec<String> {
            vec![open.unwrap_or("<none>").to_string()]
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

    /// Its output is wired from a node it does not have, so it does not load.
    const BROKEN_TOP: &str = r#"{"format_version":3,"instrument":"bt",
        "interface":{"outputs":{"out":{"from":"/osc.audio"}}},
        "nodes":[]}"#;

    /// Nests `nv.json`, which is valid on its own but names no `out` for this to wire.
    const NESTS_BROKEN: &str = r#"{"format_version":3,"instrument":"nt",
        "resources":{"v":"nv.json"},
        "interface":{"outputs":{"out":{"from":"/voice.out"}}},
        "nodes":[{"type":"subpatch","address":"/voice","patch":"v"}]}"#;

    const NESTED_WITHOUT_OUT: &str = r#"{"format_version":3,"instrument":"nv",
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

        /// The loudest sample of the last block rendered once any pending swap has landed.
        fn peak(&self) -> f32 {
            let mut slot = self.slot.borrow_mut();
            let mut buf = vec![0.0f32; 128 * slot.channels().max(1)];
            for _ in 0..BLOCKS_BETWEEN_CALLS {
                slot.fill(&mut buf);
            }
            buf.iter().fold(0.0f32, |peak, s| peak.max(s.abs()))
        }
    }

    /// Enough blocks for the render side to finish a swap, its declick ramp included.
    const BLOCKS_BETWEEN_CALLS: usize = 64;

    fn serving() -> Served {
        serving_with(Host {
            hold: false,
            only: None,
        })
    }

    fn serving_with(host: Host) -> Served {
        let files = Arc::new(Files::default());
        for (source, text) in [
            ("top.json", TOP),
            ("v.json", V),
            ("other.json", OTHER),
            ("bt.json", BROKEN_TOP),
            ("nt.json", NESTS_BROKEN),
            ("nv.json", NESTED_WITHOUT_OUT),
        ] {
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
        let s = serving_with(Host {
            hold: true,
            only: None,
        });
        let before = installed_hash(&s.state);
        match dispatch(&s.state, &open_line("other.json", None)) {
            Response::Held(held) => assert_eq!(held.requested, "other.json"),
            other => panic!("expected a held open, got {other:?}"),
        }
        assert_eq!(installed_hash(&s.state), before);
    }

    fn confined() -> Served {
        serving_with(Host {
            hold: false,
            only: Some("nowhere/"),
        })
    }

    #[test]
    fn open_of_a_name_the_server_does_not_offer_is_refused() {
        let s = confined();
        let before = installed_hash(&s.state);
        match dispatch(&s.state, &open_line("other.json", None)) {
            Response::SwapReport(report) => {
                assert!(!report.report.ok);
                assert_eq!(report.content_hash, before);
                assert_eq!(
                    report.report.errors[0].message,
                    "`other.json` is not offered by this server"
                );
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(installed_hash(&s.state), before);
    }

    #[test]
    fn a_call_read_of_a_name_the_server_does_not_offer_is_refused() {
        let s = confined();
        let line = Request::Call {
            tool: "describe_instrument".to_string(),
            args: json!({"source": "other.json"}),
        }
        .to_ndjson();
        match dispatch(&s.state, &line) {
            Response::Refused { message } => {
                assert_eq!(message, "`other.json` is not offered by this server")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        // The open instrument's own tree stays nameable.
        let (output, _) = call(&s, "describe_instrument", json!({"source": "top.json"}));
        assert!(!output.is_null());
    }

    #[test]
    fn a_create_outside_what_the_server_offers_is_refused_and_writes_nothing() {
        let s = confined();
        let line = Request::Call {
            tool: "new_instrument".to_string(),
            args: json!({"source": "fresh.json", "name": "fresh"}),
        }
        .to_ndjson();
        assert!(matches!(
            dispatch(&s.state, &line),
            Response::Refused { .. }
        ));
        assert_eq!(s.files.get("fresh.json"), None);
    }

    /// Offers only names starting with `k`, besides the open tree.
    fn offering_only_k() -> Served {
        serving_with(Host {
            hold: false,
            only: Some("k"),
        })
    }

    fn call_line(tool: &str, args: Value) -> String {
        Request::Call {
            tool: tool.to_string(),
            args,
        }
        .to_ndjson()
    }

    #[test]
    fn arguments_that_are_not_file_names_are_not_confined() {
        let s = offering_only_k();
        let before = installed_hash(&s.state);
        let (renamed, _) = call(
            &s,
            "rename_instrument_node",
            json!({"source": "top.json", "from": "/voice", "to": "/voice2"}),
        );
        assert_eq!(renamed["written"], true, "{renamed}");
        let after = installed_hash(&s.state);

        let line = call_line(
            "wire_instrument_input",
            json!({"source": "top.json", "address": "/voice2", "input": "x", "from": "/voice2.out"}),
        );
        assert!(
            matches!(dispatch(&s.state, &line), Response::Called { .. }),
            "a port path is not a file name"
        );
        s.render();
        let now = installed_hash(&s.state);

        let line = call_line(
            "restore_instrument",
            json!({"source": "top.json", "to": before, "expect": now}),
        );
        match dispatch(&s.state, &line) {
            Response::Called { output, .. } => assert_eq!(output["written"], true, "{output}"),
            other => panic!("expected a called answer, got {other:?}"),
        }
        let _ = after;
    }

    #[test]
    fn a_resource_outside_what_the_server_offers_is_refused_and_writes_nothing() {
        let s = offering_only_k();
        let before = s.files.get("top.json");
        let line = call_line(
            "add_instrument_resource",
            json!({"source": "top.json", "id": "k", "resource_source": "outside/k.wav"}),
        );
        match dispatch(&s.state, &line) {
            Response::Refused { message } => {
                assert_eq!(message, "`outside/k.wav` is not offered by this server")
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert_eq!(s.files.get("top.json"), before);
    }

    #[test]
    fn a_resource_inside_what_the_server_offers_is_written() {
        let s = offering_only_k();
        let (output, _) = call(
            &s,
            "add_instrument_resource",
            json!({"source": "top.json", "id": "k", "resource_source": "k.wav"}),
        );
        assert_eq!(output["written"], true, "{output}");
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
            Response::Listed {
                names: vec!["top.json".to_string()]
            }
        );
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
                .contains("no instrument document is open; open one with `open_instrument`"),
            "{output}"
        );
    }

    fn open_report(s: &Served, name: &str, expect: Option<String>) -> crate::engine::SwapReport {
        let report = match dispatch(&s.state, &open_line(name, expect)) {
            Response::SwapReport(report) => report,
            other => panic!("expected a swap report, got {other:?}"),
        };
        s.render();
        report
    }

    fn snapshot(s: &Served) -> crate::engine::DocumentSnapshot {
        match dispatch(&s.state, &Request::GetDocument.to_ndjson()) {
            Response::Document(snapshot) => snapshot,
            other => panic!("expected a document, got {other:?}"),
        }
    }

    #[test]
    fn open_of_a_broken_document_opens_it_silently() {
        let s = serving();
        assert!(s.peak() > 0.0, "the open instrument plays before");
        let events = s.state.subscribe();
        let report = open_report(&s, "bt.json", None);
        assert!(!report.report.ok, "{report:?}");
        assert!(!report.report.errors.is_empty(), "{report:?}");
        assert_eq!(
            report.content_hash, "",
            "nothing plays, so no hash names it"
        );
        assert!(
            report.report.warnings[0]
                .message
                .contains("nothing plays until an edit makes it load"),
            "{report:?}"
        );

        assert_eq!(s.peak(), 0.0, "the silent Plan plays");
        assert_eq!(
            s.state.try_installed(),
            Installed::Known {
                input_channels: 0,
                content_hash: String::new(),
                source: Some("bt.json".to_string()),
            }
        );
        let snapshot = snapshot(&s);
        assert_eq!(snapshot.source.as_deref(), Some("bt.json"));
        assert_eq!(snapshot.content_hash, "");
        assert_eq!(snapshot.document, Value::Null);
        assert_eq!(s.state.tree(), BTreeSet::from(["bt.json".to_string()]));

        assert_eq!(
            events.try_recv(),
            Ok(Event::Opened {
                name: "bt.json".to_string(),
                hash: String::new(),
            })
        );
        assert_eq!(
            events.try_recv(),
            Ok(Event::Invalid {
                errors: report.report.errors.clone(),
            })
        );
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn an_edit_that_fixes_the_open_broken_document_starts_playing() {
        let s = serving();
        open_report(&s, "bt.json", None);
        let events = s.state.subscribe();
        let (output, _) = call(
            &s,
            "add_instrument_node",
            json!({"source": "bt.json", "type": "oscillator", "address": "/osc"}),
        );
        assert_eq!(output["written"], true, "{output}");
        assert_eq!(json!(installed_hash(&s.state)), output["hash"]);
        assert_eq!(
            events.try_recv(),
            Ok(Event::Changed {
                hash: output["hash"].as_str().expect("hash").to_string(),
                by: ChangedBy::Client,
            })
        );
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        assert!(s.peak() > 0.0, "it plays");
        assert_eq!(snapshot(&s).content_hash, installed_hash(&s.state));
    }

    #[test]
    fn an_edit_that_leaves_it_broken_is_written_but_nothing_plays() {
        let s = serving();
        let opened = open_report(&s, "nt.json", None);
        assert!(!opened.report.ok, "{opened:?}");
        assert_eq!(
            s.state.tree(),
            BTreeSet::from(["nt.json".to_string(), "nv.json".to_string()]),
            "the nested file the open read is part of the open instrument"
        );
        let events = s.state.subscribe();
        let (output, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "nv.json", "name": "renamed"}),
        );
        assert_eq!(output["written"], true, "{output}");
        assert!(
            output["notes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n.as_str().unwrap().contains("nothing plays until")),
            "{output}"
        );
        assert!(s.files.get("nv.json").unwrap().contains("renamed"));
        assert!(
            matches!(events.try_recv(), Ok(Event::Invalid { errors }) if !errors.is_empty()),
            "an edit that leaves it broken says what is still wrong"
        );
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
        assert_eq!(installed_hash(&s.state), "");
        assert_eq!(s.peak(), 0.0);
    }

    #[test]
    fn once_playing_the_strict_rule_is_back() {
        let s = serving();
        open_report(&s, "nt.json", None);
        let (fixed, _) = call(
            &s,
            "add_instrument_interface_output",
            json!({"source": "nv.json", "name": "out", "from": "/osc.audio"}),
        );
        assert_eq!(fixed["written"], true, "{fixed}");
        assert!(s.peak() > 0.0, "the fix to the nested file makes it play");
        let playing = installed_hash(&s.state);
        assert_ne!(playing, "");
        let written = s.files.get("nv.json").unwrap();

        let (broken, _) = call(
            &s,
            "remove_instrument_interface_output",
            json!({"source": "nv.json", "name": "out"}),
        );
        assert_eq!(broken["written"], false, "{broken}");
        assert_eq!(s.files.get("nv.json").unwrap(), written);
        assert_eq!(installed_hash(&s.state), playing);
    }

    #[test]
    fn an_expect_on_open_of_a_broken_document_is_checked_against_what_plays() {
        let s = serving();
        let playing = installed_hash(&s.state);
        match dispatch(&s.state, &open_line("bt.json", Some("0badc0de".into()))) {
            Response::Conflict(conflict) => assert_eq!(conflict.actual, playing),
            other => panic!("expected a conflict, got {other:?}"),
        }
        assert_eq!(installed_hash(&s.state), playing);
        assert!(s.peak() > 0.0, "a conflict unloads nothing");

        let report = open_report(&s, "bt.json", Some(playing.clone()));
        assert_eq!(report.content_hash, "");
        // Nothing plays now, and that is what a later `expect` is compared against.
        match dispatch(&s.state, &open_line("other.json", Some(playing))) {
            Response::Conflict(conflict) => assert_eq!(conflict.actual, ""),
            other => panic!("expected a conflict, got {other:?}"),
        }
        let report = open_report(&s, "other.json", Some(String::new()));
        assert!(report.report.ok, "{report:?}");
    }

    #[test]
    fn a_client_open_of_a_broken_document_is_held_for_a_holding_host() {
        let s = serving_with(Host {
            hold: true,
            only: None,
        });
        let before = s.state.try_installed();
        let events = s.state.subscribe();
        match dispatch(&s.state, &open_line("bt.json", None)) {
            Response::Held(held) => assert_eq!(held.requested, "bt.json"),
            other => panic!("expected a held open, got {other:?}"),
        }
        assert_eq!(s.state.try_installed(), before);
        assert!(s.peak() > 0.0);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn a_refused_open_while_nothing_plays_names_what_is_still_open() {
        let s = serving();
        open_report(&s, "bt.json", None);
        let report = open_report(&s, "missing.json", None);
        assert!(!report.report.ok);
        assert_eq!(report.content_hash, "");
        assert!(
            report.report.errors[0].message.contains("missing.json"),
            "{report:?}"
        );
        assert_eq!(
            report.report.warnings[0].message,
            "nothing was opened; bt.json is still open but does not load, so nothing plays"
        );
        assert_eq!(s.state.tree(), BTreeSet::from(["bt.json".to_string()]));
    }

    #[test]
    fn a_refused_swap_while_nothing_plays_says_nothing_was_swapped_in() {
        let s = serving();
        open_report(&s, "bt.json", None);
        let line = Request::Swap {
            source: DocSource::Path("missing.json".to_string()),
            expect: None,
        }
        .to_ndjson();
        let report = match dispatch(&s.state, &line) {
            Response::SwapReport(report) => report,
            other => panic!("expected a swap report, got {other:?}"),
        };
        assert!(!report.report.ok);
        assert_eq!(report.content_hash, "");
        assert_eq!(
            report.report.warnings[0].message,
            "nothing was swapped in; bt.json is still open but does not load, so nothing plays"
        );
    }

    #[test]
    fn an_edit_while_the_silence_swap_is_in_flight_says_nothing_plays() {
        let s = serving();
        // Nothing is rendered after the open, so its swap to silence stays in flight.
        assert!(matches!(
            dispatch(&s.state, &open_line("bt.json", None)),
            Response::SwapReport(_)
        ));
        let (output, _) = call_undrained(
            &s.state,
            "add_instrument_node",
            json!({"source": "bt.json", "type": "oscillator", "address": "/osc"}),
        );
        assert_eq!(output["written"], false, "{output}");
        let note = output["notes"][0].as_str().unwrap();
        assert!(
            note.contains("nothing plays, since bt.json does not load"),
            "{output}"
        );
        assert!(!note.contains("keeps playing"), "{output}");
        assert_eq!(s.files.get("bt.json").unwrap(), BROKEN_TOP);
    }

    #[test]
    fn opening_a_valid_document_while_a_broken_one_is_open_plays_it() {
        let s = serving();
        open_report(&s, "bt.json", None);
        let events = s.state.subscribe();
        let report = open_report(&s, "other.json", None);
        assert!(report.report.ok, "{report:?}");
        let hash = installed_hash(&s.state);
        assert_ne!(hash, "");
        assert_eq!(report.content_hash, hash);
        assert_eq!(
            events.try_recv(),
            Ok(Event::Opened {
                name: "other.json".to_string(),
                hash: hash.clone(),
            })
        );
        assert!(s.peak() > 0.0);
        let snapshot = snapshot(&s);
        assert_eq!(snapshot.content_hash, hash);
        assert_ne!(snapshot.document, Value::Null);
        assert_eq!(s.state.tree(), BTreeSet::from(["other.json".to_string()]));
    }

    #[test]
    fn an_outside_fix_to_the_open_broken_document_starts_playing() {
        let s = serving();
        open_report(&s, "nt.json", None);
        let events = s.state.subscribe();
        // The same broken files read again: already reported.
        assert_eq!(s.state.reload(), None);
        s.files
            .write_text("nv.json", &V.replace(r#""v""#, r#""nv""#))
            .unwrap();
        let reloaded = s.state.reload();
        assert!(
            matches!(
                reloaded,
                Some(Event::Changed {
                    by: ChangedBy::Outside,
                    ..
                })
            ),
            "{reloaded:?}"
        );
        assert_eq!(events.try_recv().ok(), reloaded);
        assert!(s.peak() > 0.0);
        assert_ne!(installed_hash(&s.state), "");
    }

    #[test]
    fn edit_publishes_changed_by_client() {
        let s = serving();
        let events = InProcess::new(s.state.clone()).subscribe();
        let (output, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "top.json", "name": "renamed"}),
        );
        assert_eq!(output["written"], true, "{output}");
        assert_eq!(
            events.try_recv().expect("an event"),
            Event::Changed {
                hash: output["hash"].as_str().expect("hash").to_string(),
                by: ChangedBy::Client,
            }
        );
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn refused_edit_publishes_nothing() {
        let s = serving();
        let events = s.state.subscribe();
        let (output, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "other.json", "name": "renamed"}),
        );
        assert_eq!(output["written"], false, "{output}");
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn open_publishes_opened() {
        let s = serving();
        let events = s.state.subscribe();
        match dispatch(&s.state, &open_line("other.json", None)) {
            Response::SwapReport(report) => assert!(report.report.ok, "{:?}", report.report.errors),
            other => panic!("expected a swap report, got {other:?}"),
        }
        s.render();
        assert_eq!(
            events.try_recv().expect("an event"),
            Event::Opened {
                name: "other.json".to_string(),
                hash: installed_hash(&s.state),
            }
        );
        // Opening what is already open is still an open.
        match dispatch(&s.state, &open_line("other.json", None)) {
            Response::SwapReport(report) => assert!(report.report.ok, "{:?}", report.report.errors),
            other => panic!("expected a swap report, got {other:?}"),
        }
        s.render();
        assert!(matches!(
            events.try_recv().expect("an event"),
            Event::Opened { .. }
        ));
    }

    #[test]
    fn a_swap_publishes_opened_for_a_new_source_and_changed_for_the_same_one() {
        let s = serving();
        let events = s.state.subscribe();
        let by_value = Request::Swap {
            source: crate::engine::DocSource::Document(serde_json::from_str(OTHER).unwrap()),
            expect: None,
        }
        .to_ndjson();
        for _ in 0..2 {
            match dispatch(&s.state, &by_value) {
                Response::SwapReport(report) => assert!(report.report.ok, "{report:?}"),
                other => panic!("expected a swap report, got {other:?}"),
            }
            s.render();
        }
        let hash = installed_hash(&s.state);
        // From `top.json` to by value is a new source; by value again is the same one.
        assert_eq!(
            events.try_recv().expect("first"),
            Event::Opened {
                name: String::new(),
                hash: hash.clone(),
            }
        );
        assert_eq!(
            events.try_recv().expect("second"),
            Event::Changed {
                hash,
                by: ChangedBy::Client,
            }
        );
    }

    #[test]
    fn a_held_or_rejected_swap_publishes_nothing() {
        let s = serving_with(Host {
            hold: true,
            only: None,
        });
        let events = s.state.subscribe();
        assert!(matches!(
            dispatch(&s.state, &open_line("other.json", None)),
            Response::Held(_)
        ));
        match dispatch(&s.state, &open_line("missing.json", None)) {
            Response::SwapReport(report) => assert!(!report.report.ok),
            other => panic!("expected a swap report, got {other:?}"),
        }
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn reload_after_outside_edit_swaps_and_reports_outside() {
        let s = serving();
        let events = s.state.subscribe();
        s.files
            .write_text("top.json", &TOP.replace(r#""top""#, r#""edited""#))
            .unwrap();
        let reloaded = s.state.reload();
        s.render();
        let Some(Event::Changed { hash, by }) = reloaded.clone() else {
            panic!("expected a change, got {reloaded:?}");
        };
        assert_eq!(by, ChangedBy::Outside);
        assert_eq!(installed_hash(&s.state), hash);
        assert_eq!(events.try_recv().expect("an event"), reloaded.unwrap());
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn reload_of_invalid_tree_keeps_playing() {
        let s = serving();
        let events = s.state.subscribe();
        let before = installed_hash(&s.state);
        s.files.write_text("v.json", "{ not json").unwrap();
        let reloaded = s.state.reload();
        assert!(
            matches!(&reloaded, Some(Event::Invalid { errors }) if !errors.is_empty()),
            "{reloaded:?}"
        );
        assert_eq!(installed_hash(&s.state), before);
        assert_eq!(events.try_recv().expect("an event"), reloaded.unwrap());
    }

    #[test]
    fn an_unchanged_invalid_tree_is_reported_once() {
        let s = serving();
        let events = s.state.subscribe();
        s.files.write_text("v.json", "{ not json").unwrap();
        assert!(matches!(s.state.reload(), Some(Event::Invalid { .. })));
        assert_eq!(s.state.reload(), None);
        assert!(events.try_recv().is_ok());
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));

        // Put back as it plays, then broken the same way again: a new failure to report.
        s.files.write_text("v.json", V).unwrap();
        assert!(matches!(s.state.reload(), Some(Event::Changed { .. })));
        s.files.write_text("v.json", "{ not json").unwrap();
        assert!(matches!(s.state.reload(), Some(Event::Invalid { .. })));
    }

    #[test]
    fn own_write_is_not_reported_as_outside() {
        let s = serving();
        let (output, _) = call(
            &s,
            "set_instrument_name",
            json!({"source": "top.json", "name": "renamed"}),
        );
        assert_eq!(output["written"], true, "{output}");
        let events = s.state.subscribe();
        assert_eq!(s.state.reload(), None);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn reload_while_a_swap_is_in_flight_installs_nothing() {
        let s = serving();
        let (first, _) = call_undrained(
            &s.state,
            "set_instrument_name",
            json!({"source": "top.json", "name": "one"}),
        );
        assert_eq!(first["written"], true, "{first}");
        s.files
            .write_text("top.json", &TOP.replace(r#""top""#, r#""outside""#))
            .unwrap();
        assert_eq!(s.state.reload(), None);
        assert_eq!(json!(installed_hash(&s.state)), first["hash"]);

        // Once the render side has taken the swap home, the same files go live.
        s.render();
        assert!(matches!(
            s.state.reload(),
            Some(Event::Changed {
                by: ChangedBy::Outside,
                ..
            })
        ));
    }

    #[test]
    fn a_reload_while_a_swap_is_in_flight_says_so_and_reads_nothing() {
        let s = serving();
        let (first, _) = call_undrained(
            &s.state,
            "set_instrument_name",
            json!({"source": "top.json", "name": "one"}),
        );
        assert_eq!(first["written"], true, "{first}");
        s.files
            .write_text("top.json", &TOP.replace(r#""top""#, r#""outside""#))
            .unwrap();
        assert_eq!(s.state.try_reload(), Err(SwapInFlight));
        s.render();
        assert!(matches!(
            s.state.try_reload(),
            Ok(Some(Event::Changed {
                by: ChangedBy::Outside,
                ..
            }))
        ));
    }

    #[test]
    fn reload_with_nothing_open_reports_nothing() {
        let mut s = serving();
        s.state = s.state.clone().with_installed_source(None);
        s.files.write_text("top.json", "{ not json").unwrap();
        assert_eq!(s.state.reload(), None);
    }

    #[test]
    fn reload_of_untouched_files_reports_nothing() {
        let s = serving();
        let events = s.state.subscribe();
        assert_eq!(s.state.reload(), None);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn reload_follows_an_outside_edit_to_a_nested_document() {
        let s = serving();
        let events = s.state.subscribe();
        let before = installed_hash(&s.state);
        s.files
            .write_text("v.json", &V.replace("/osc", "/osc2"))
            .unwrap();
        let reloaded = s.state.reload();
        s.render();
        assert_eq!(
            reloaded,
            Some(Event::Changed {
                hash: before,
                by: ChangedBy::Outside,
            }),
            "the top document is unchanged, so its hash is too"
        );
        assert_eq!(events.try_recv().expect("an event"), reloaded.unwrap());
        // What plays now is what is on disk: reading it again changes nothing.
        assert_eq!(s.state.reload(), None);
    }

    #[test]
    fn a_tree_that_plans_again_after_invalid_reports_changed() {
        let s = serving();
        let events = s.state.subscribe();
        let playing = installed_hash(&s.state);
        s.files.write_text("v.json", "{ not json").unwrap();
        assert!(matches!(s.state.reload(), Some(Event::Invalid { .. })));
        s.files.write_text("v.json", V).unwrap();
        let recovered = Event::Changed {
            hash: playing.clone(),
            by: ChangedBy::Outside,
        };
        assert_eq!(s.state.reload(), Some(recovered.clone()));
        assert_eq!(installed_hash(&s.state), playing);
        assert!(matches!(events.try_recv(), Ok(Event::Invalid { .. })));
        assert_eq!(events.try_recv(), Ok(recovered));
        // Recovered once: the next read of the same files is quiet.
        assert_eq!(s.state.reload(), None);
        assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    }
}
