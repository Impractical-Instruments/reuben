//! The editor's one write path: every change to a document is a `reuben-api` authoring verb called
//! through [`write`].

use reuben_api::authoring::{Answer, EditResult, Refusal, Resources};
use reuben_api::engine::Channel;

use crate::session::Session;

/// Run one authoring verb on the document `source` of the open instrument.
///
/// `verb` is handed the store scoped to that document and the `expect` to pass: the content hash
/// the session last read for it, so a write made against a view that `reuben-mcp` has since
/// changed is answered as a conflict instead of landing on a document the user was not looking at.
///
/// `source` is a document's `source` as the session's tree spells it; any other is refused
/// without running `verb`, there being no hash to guard the write with.
///
/// The session reads the files again before this returns, so its tree is what the verb left on
/// disk, written or not, and a written change is already on its way to the engine. The next
/// [`Session::poll`] reports the reload. An `Ok` whose `written` is false is a rejected edit or a
/// conflict; its report says which, on the node and port at fault.
pub fn write(
    session: &mut Session,
    channel: &dyn Fn() -> Option<Channel>,
    source: &str,
    verb: impl FnOnce(&dyn Resources, Option<String>) -> Result<Answer<EditResult>, Refusal>,
) -> Result<EditResult, Refusal> {
    let Some(document) = session.tree().document(source) else {
        return Err(Refusal {
            message: format!("{source} is not a file of the open instrument"),
        });
    };
    let expect = document.hash().map(str::to_string);
    let result = verb(&session.store(source), expect);
    session.refresh(channel);
    result.map(|answer| answer.output)
}

/// [`write`], guarded by `expect` rather than by the session's hash of `source` now: for an edit
/// the user began against an earlier read of the document, such as a value held while the file
/// changed under it. A document changed since `expect` is answered as a conflict.
pub fn write_against(
    session: &mut Session,
    channel: &dyn Fn() -> Option<Channel>,
    source: &str,
    expect: String,
    verb: impl FnOnce(&dyn Resources, Option<String>) -> Result<Answer<EditResult>, Refusal>,
) -> Result<EditResult, Refusal> {
    if session.tree().document(source).is_none() {
        return Err(Refusal {
            message: format!("{source} is not a file of the open instrument"),
        });
    }
    let result = verb(&session.store(source), Some(expect));
    session.refresh(channel);
    result.map(|answer| answer.output)
}

#[cfg(test)]
mod tests {
    use reuben_api::authoring::{
        set_instrument_input, set_instrument_name, SetInstrumentInput, SetInstrumentName,
    };
    use reuben_api::FsResolver;
    use reuben_native::test_support::within;
    use serde_json::json;

    use super::*;
    use crate::session::test_support::Rig;
    use crate::session::Status;

    const HANG_SECS: u64 = 60;

    fn set_cutoff(rig: &mut Rig, source: &str, cutoff: f64) -> EditResult {
        let channel = rig.channel_source();
        write(&mut rig.session, &channel, source, |store, expect| {
            set_instrument_input(
                &SetInstrumentInput {
                    source: source.to_string(),
                    address: "/filter".to_string(),
                    input: "cutoff".to_string(),
                    value: json!(cutoff),
                    expect,
                },
                store,
            )
        })
        .expect("the verb ran")
    }

    fn voice_source(rig: &Rig) -> String {
        rig.session.tree().documents()[1].source.clone()
    }

    #[test]
    fn an_editor_write_to_a_voice_file_swaps_once_and_not_again_when_the_watcher_reports_it() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("edit_once", "default.json");
            let voice = voice_source(&rig);

            let edit = set_cutoff(&mut rig, &voice, 1200.0);
            assert!(edit.written, "{:?}", edit.report);
            assert_eq!(
                rig.session.tree().document(&voice).and_then(|d| d.hash()),
                Some(edit.hash.as_str()),
                "the session holds what the verb wrote before the watcher says anything"
            );

            rig.poll_until("the watcher reporting the editor's own write", |rig| {
                rig.touches() > 0
            });
            rig.settle();
            assert_eq!(rig.swaps(), 1);
            assert_eq!(rig.status(), &Status::Current);
        });
    }

    #[test]
    fn a_write_against_a_view_the_sidecar_has_changed_is_a_conflict_and_the_view_catches_up() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("edit_conflict", "default.json");
            let voice = voice_source(&rig);

            // The sidecar's write, which the session has not read yet.
            let theirs = set_instrument_name(
                &SetInstrumentName {
                    source: voice.clone(),
                    name: "renamed-by-the-sidecar".to_string(),
                    expect: None,
                },
                &FsResolver::for_document(&voice).stat_only(),
            )
            .expect("the verb ran")
            .output;
            assert!(theirs.written);

            let ours = set_cutoff(&mut rig, &voice, 1200.0);
            assert!(!ours.written, "{:?}", ours.report);
            assert_eq!(ours.hash, theirs.hash, "nothing of ours landed");
            assert_eq!(
                rig.session.tree().document(&voice).and_then(|d| d.hash()),
                Some(theirs.hash.as_str())
            );

            // The same gesture again is made against what is on disk now.
            let again = set_cutoff(&mut rig, &voice, 1200.0);
            assert!(again.written, "{:?}", again.report);
            rig.settle();
            assert_eq!(rig.status(), &Status::Current);
        });
    }

    #[test]
    fn a_source_the_tree_does_not_hold_is_refused_without_running_the_verb() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("edit_unknown", "default.json");
            let voice = voice_source(&rig);
            let respelled = voice.replace("default-voice.json", "../voices/default-voice.json");
            assert_ne!(respelled, voice);
            let channel = rig.channel_source();
            let edit = write(&mut rig.session, &channel, &respelled, |_, _| {
                panic!("the verb ran with nothing to guard it")
            });
            assert!(edit.is_err(), "{edit:?}");
        });
    }

    #[test]
    fn a_rejected_edit_writes_nothing_and_swaps_nothing() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("edit_rejected", "default.json");
            let voice = voice_source(&rig);
            let before = rig.session.tree().clone();

            let channel = rig.channel_source();
            let edit = write(&mut rig.session, &channel, &voice, |store, expect| {
                set_instrument_input(
                    &SetInstrumentInput {
                        source: voice.clone(),
                        address: "/no-such-node".to_string(),
                        input: "cutoff".to_string(),
                        value: json!(1.0),
                        expect,
                    },
                    store,
                )
            });
            assert!(!matches!(&edit, Ok(result) if result.written), "{edit:?}");
            rig.settle();
            assert_eq!(rig.swaps(), 0);
            assert_eq!(rig.session.tree(), &before);
        });
    }
}
