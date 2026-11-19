//! The editor's one write path: every change to a document is a `reuben-api` authoring verb called
//! through [`write`](fn@write), and every return to an earlier state is a restore through [`restore`].

pub mod local_copy;

use std::path::Path;

use reuben_api::authoring::{restore_instrument, Answer, EditResult, Refusal, Resources};
use reuben_api::engine::Channel;

use crate::feedback::Feedback;
use crate::session::{Direction, Session, Step};

/// Run one authoring verb on the document `source` of the open instrument.
///
/// `verb` is handed the store scoped to that document and the `expect` to pass: the content hash
/// the session last read for it, so a write made against a view that `reuben-mcp` has since
/// changed is answered as a conflict instead of landing on a document the user was not looking at.
///
/// `source` is a document's `source` as the session's tree spells it; any other is refused
/// without running `verb`, there being no hash to guard the write with.
///
/// A change that is written is one undo step, and clears what could be redone.
///
/// The session reads the files again before this returns, so its tree is what the verb left on
/// disk, written or not, and a written change is already on its way to the engine. The next
/// [`Session::poll`] reports the reload. An `Ok` whose `written` is false is a rejected edit or a
/// conflict; its report says which, on the node and port at fault. Either way, and for a verb
/// that could not run, the session holds the [`Feedback`] until an edit lands.
pub fn write(
    session: &mut Session,
    channel: &dyn Fn() -> Option<Channel>,
    source: &str,
    verb: impl FnOnce(&dyn Resources, Option<String>) -> Result<Answer<EditResult>, Refusal>,
) -> Result<EditResult, Refusal> {
    let Some(document) = session.tree().document(source) else {
        return not_a_file(session, source);
    };
    let expect = document.hash().map(str::to_string);
    run(session, channel, source, expect, verb)
}

fn not_a_file(session: &mut Session, source: &str) -> Result<EditResult, Refusal> {
    let refused = Err(Refusal {
        message: format!("{source} is not a file of the open instrument"),
    });
    *session.feedback_mut() = Feedback::of(source, None, &refused);
    refused
}

/// Run `verb` on `source` through the session's history store, guarded by `expect`, and record a
/// written change as one undo step from `expect`.
fn run(
    session: &mut Session,
    channel: &dyn Fn() -> Option<Channel>,
    source: &str,
    expect: Option<String>,
    verb: impl FnOnce(&dyn Resources, Option<String>) -> Result<Answer<EditResult>, Refusal>,
) -> Result<EditResult, Refusal> {
    let store: &dyn Resources = session.history(source);
    #[cfg(test)]
    let counted = test_support::Counted(store);
    #[cfg(test)]
    let store: &dyn Resources = &counted;
    let result = verb(store, expect.clone()).map(|answer| answer.output);
    *session.feedback_mut() = Feedback::of(source, expect.as_deref(), &result);
    // A written edit landed on `expect`: the guard refuses any other.
    if let (Ok(edit), Some(before)) = (&result, expect) {
        if edit.written {
            session.undo_mut().record(Step {
                source: source.to_string(),
                before,
                after: edit.hash.clone(),
            });
        }
    }
    session.refresh(channel);
    result
}

/// What one [`restore`] did.
#[derive(Debug)]
pub enum Restored {
    /// There was nothing to undo or redo.
    Nothing,
    /// The file is in the state the step names, and the step has crossed to the other stack.
    Landed,
    /// The file was written by someone else since the step was recorded and is left as they
    /// left it. Nothing was written, and the step stays: it lands again if the file returns to
    /// the state the step expects.
    Conflict { step: Step },
    /// The state the step names no longer validates, a file it references having changed since.
    /// Nothing was written, and the step stays: it lands again once that file is put back.
    Rejected { step: Step, result: EditResult },
    /// The state the step names is no longer held. It, and every step behind it in the same
    /// direction — `dropped` in all — are gone.
    Forgotten { step: Step, dropped: usize },
    /// The restore could not run: the file does not read as a document, or it could not be read
    /// or written. The step stays, to land once that is put right.
    Refused { step: Step, why: String },
}

impl Restored {
    /// What to tell the user, when it did not simply land.
    pub fn note(&self, direction: Direction) -> Option<String> {
        let verb = match direction {
            Direction::Undo => "undo",
            Direction::Redo => "redo",
        };
        match self {
            Restored::Nothing | Restored::Landed => None,
            Restored::Conflict { step } => Some(format!(
                "Cannot {verb}: {} was changed outside the editor since, and is left as it is.",
                file_name(&step.source)
            )),
            Restored::Rejected { step, result } => {
                let mut note = format!(
                    "Cannot {verb}: {} would not load in that state:",
                    file_name(&step.source)
                );
                for error in &result.report.errors {
                    note.push('\n');
                    for at in [&error.node, &error.port].into_iter().flatten() {
                        note.push_str(at);
                        note.push_str(": ");
                    }
                    note.push_str(&error.message);
                }
                Some(note)
            }
            Restored::Forgotten { step, dropped } => Some(format!(
                "Cannot {verb}: that state of {} is no longer kept. {dropped} step(s) dropped.",
                file_name(&step.source)
            )),
            Restored::Refused { step, why } => {
                Some(format!("Cannot {verb} {}: {why}", file_name(&step.source)))
            }
        }
    }
}

fn file_name(source: &str) -> String {
    Path::new(source).file_name().map_or_else(
        || source.to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Undo or redo the editor's next step in `direction`: return its file to the state on the far
/// side of it, guarded by the hash on this side.
///
/// The session reads the files again before this returns, as after [`write`](fn@write), and holds the
/// outcome's [`note`](Restored::note) for the window to show.
pub fn restore(
    session: &mut Session,
    channel: &dyn Fn() -> Option<Channel>,
    direction: Direction,
) -> Restored {
    let Some(step) = session.undo().next(direction).cloned() else {
        return Restored::Nothing;
    };
    let expect = step.expect(direction).to_string();
    let restored = match restore_instrument(&step.restore(direction), session.history(&step.source))
    {
        Ok(answer) if answer.output.written => {
            session.undo_mut().crossed(direction);
            *session.feedback_mut() = None;
            Restored::Landed
        }
        Ok(answer) if answer.output.hash != expect => Restored::Conflict { step },
        Ok(answer) => Restored::Rejected {
            step,
            result: answer.output,
        },
        // Only a state the store no longer holds is gone for good; a file that would not read
        // or write refuses the same way and is worth trying again.
        Err(refusal) => {
            let to = step.restore(direction).to;
            if session.history(&step.source).holds(&step.source, &to) {
                Restored::Refused {
                    step,
                    why: refusal.message,
                }
            } else {
                let dropped = session.undo_mut().forget(direction);
                Restored::Forgotten { step, dropped }
            }
        }
    };
    let note = restored.note(direction);
    session.undo_mut().set_note(note);
    session.refresh(channel);
    restored
}

/// [`write`](fn@write), guarded by `expect` rather than by the session's hash of `source` now: for an edit
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
        return not_a_file(session, source);
    }
    run(session, channel, source, Some(expect), verb)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::Cell;

    use reuben_api::authoring::{Created, ResolveError, Resources, SampleBuffer};

    thread_local! {
        static WRITES: Cell<usize> = const { Cell::new(0) };
    }

    /// How many documents [`write`](super::write) has written on this thread: one per verb call
    /// that lands, so a gesture making two verb calls shows as two.
    pub fn writes() -> usize {
        WRITES.with(Cell::get)
    }

    /// The session's store, counting what is written through it.
    pub struct Counted<'a>(pub &'a dyn Resources);

    impl Resources for Counted<'_> {
        fn read_samples(&self, source: &str) -> Result<SampleBuffer, ResolveError> {
            self.0.read_samples(source)
        }

        fn read_text(&self, source: &str) -> Result<String, ResolveError> {
            self.0.read_text(source)
        }

        fn write_text(&self, source: &str, text: &str) -> Result<(), ResolveError> {
            let written = self.0.write_text(source, text);
            if written.is_ok() {
                WRITES.with(|w| w.set(w.get() + 1));
            }
            written
        }

        fn create_text(&self, source: &str, text: &str) -> Result<Created, ResolveError> {
            let created = self.0.create_text(source, text);
            if matches!(created, Ok(Created::Written)) {
                WRITES.with(|w| w.set(w.get() + 1));
            }
            created
        }

        fn canonical(&self, source: &str, referrer: Option<&str>) -> String {
            self.0.canonical(source, referrer)
        }
    }
}

#[cfg(test)]
mod tests {
    use reuben_api::authoring::{
        add_instrument_interface_input, remove_instrument_interface_input,
        set_instrument_description, set_instrument_input, set_instrument_name,
        wire_instrument_input, AddInstrumentInterfaceInput, RemoveInstrumentInterfacePipe,
        SetInstrumentDescription, SetInstrumentInput, SetInstrumentName, WireInstrumentInput,
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
    fn a_write_against_an_earlier_read_is_one_undo_step_that_restores_that_read() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("edit_against_undo", "default.json");
            let voice = voice_source(&rig);
            let before = (rig.session.tree().document(&voice))
                .and_then(|d| d.hash())
                .expect("the voice reads")
                .to_string();

            let channel = rig.channel_source();
            let edit = write_against(
                &mut rig.session,
                &channel,
                &voice,
                before.clone(),
                |store, expect| {
                    set_instrument_input(
                        &SetInstrumentInput {
                            source: voice.clone(),
                            address: "/filter".to_string(),
                            input: "cutoff".to_string(),
                            value: json!(1500.0),
                            expect,
                        },
                        store,
                    )
                },
            )
            .expect("the verb ran");
            assert!(edit.written, "{:?}", edit.report);

            let restored = restore(&mut rig.session, &channel, Direction::Undo);
            assert!(matches!(restored, Restored::Landed), "{restored:?}");
            assert_eq!(
                rig.session.tree().document(&voice).and_then(|d| d.hash()),
                Some(before.as_str())
            );
        });
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

    #[test]
    fn a_rejection_is_let_go_once_another_hand_changes_the_file_it_was_found_in() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("edit_rejection_outlived", "default.json");
            let top = rig.session.tree().top().source.clone();
            let channel = rig.channel_source();
            // A note pipe into an audio input: the wire's types do not match.
            let rejected = write(&mut rig.session, &channel, &top, |store, expect| {
                wire_instrument_input(
                    &WireInstrumentInput {
                        source: top.clone(),
                        address: "/out".to_string(),
                        input: "audio".to_string(),
                        from: "/notes".to_string(),
                        expect,
                    },
                    store,
                )
            });
            assert!(!matches!(&rejected, Ok(result) if result.written));
            assert!(
                matches!(rig.session.feedback(), Some(Feedback::Rejected { .. })),
                "{:?}",
                rig.session.feedback()
            );

            sidecar_describes(&top, "the sidecar's");
            rig.poll_until("the session reading the sidecar's write", |rig| {
                rig.session.feedback().is_none()
            });
        });
    }

    // --- undo and redo -------------------------------------------------------------------------

    /// `euclidean-drums` and the `dj-strip` subpatch file its strips are.
    fn drums(test: &str) -> (Rig, String, String) {
        let rig = Rig::open(test, "euclidean-drums.json");
        let top = rig.session.tree().top().source.clone();
        let strip = rig
            .session
            .tree()
            .documents()
            .iter()
            .find(|d| d.path.ends_with("patches/dj-strip.json"))
            .expect("the subpatch file is in the tree")
            .source
            .clone();
        (rig, top, strip)
    }

    fn hash(rig: &Rig, source: &str) -> String {
        rig.session
            .tree()
            .document(source)
            .and_then(|d| d.hash())
            .expect("the document reads")
            .to_string()
    }

    fn edit(
        rig: &mut Rig,
        source: &str,
        verb: impl FnOnce(&dyn Resources, Option<String>) -> Result<Answer<EditResult>, Refusal>,
    ) {
        let channel = rig.channel_source();
        let result = write(&mut rig.session, &channel, source, verb).expect("the verb ran");
        assert!(result.written, "{:?}", result.report);
    }

    fn set_trim(rig: &mut Rig, top: &str, trim: f64) {
        edit(rig, top, |store, expect| {
            set_instrument_input(
                &SetInstrumentInput {
                    source: top.to_string(),
                    address: "/trim_cv".to_string(),
                    input: "in".to_string(),
                    value: json!(trim),
                    expect,
                },
                store,
            )
        });
    }

    fn rename(rig: &mut Rig, source: &str, name: &str) {
        edit(rig, source, |store, expect| {
            set_instrument_name(
                &SetInstrumentName {
                    source: source.to_string(),
                    name: name.to_string(),
                    expect,
                },
                store,
            )
        });
    }

    fn describe(rig: &mut Rig, source: &str, description: &str) {
        edit(rig, source, |store, expect| {
            set_instrument_description(
                &SetInstrumentDescription {
                    source: source.to_string(),
                    description: Some(description.to_string()),
                    expect,
                },
                store,
            )
        });
    }

    fn undo(rig: &mut Rig) -> Restored {
        let channel = rig.channel_source();
        restore(&mut rig.session, &channel, Direction::Undo)
    }

    fn redo(rig: &mut Rig) -> Restored {
        let channel = rig.channel_source();
        restore(&mut rig.session, &channel, Direction::Redo)
    }

    /// The hashes of the top-level file and the subpatch file, as the session last read them.
    fn both(rig: &Rig, top: &str, strip: &str) -> (String, String) {
        (hash(rig, top), hash(rig, strip))
    }

    #[test]
    fn undo_and_redo_walk_mixed_writes_to_the_top_level_file_and_a_subpatch_file() {
        within(HANG_SECS, || {
            let (mut rig, top, strip) = drums("undo_walk");
            let mut states = vec![both(&rig, &top, &strip)];
            set_trim(&mut rig, &top, 0.5);
            states.push(both(&rig, &top, &strip));
            rename(&mut rig, &strip, "renamed-strip");
            states.push(both(&rig, &top, &strip));
            describe(&mut rig, &top, "a description");
            states.push(both(&rig, &top, &strip));
            describe(&mut rig, &strip, "a strip description");
            states.push(both(&rig, &top, &strip));
            let mut distinct = states.clone();
            distinct.dedup();
            assert_eq!(distinct.len(), 5, "each write changed one file");

            for state in states.iter().rev().skip(1) {
                assert!(matches!(undo(&mut rig), Restored::Landed));
                assert_eq!(&both(&rig, &top, &strip), state);
            }
            assert!(matches!(undo(&mut rig), Restored::Nothing));

            for state in states.iter().skip(1) {
                assert!(matches!(redo(&mut rig), Restored::Landed));
                assert_eq!(&both(&rig, &top, &strip), state);
            }
            assert!(matches!(redo(&mut rig), Restored::Nothing));
            assert_eq!(rig.session.undo().note(), None);

            rig.settle();
            assert_eq!(rig.status(), &Status::Current);
            assert_eq!(rig.playing(), hash(&rig, &top), "the engine plays the file");
        });
    }

    #[test]
    fn a_new_write_after_an_undo_clears_the_redo() {
        within(HANG_SECS, || {
            let (mut rig, top, _) = drums("undo_branch");
            set_trim(&mut rig, &top, 0.5);
            set_trim(&mut rig, &top, 0.6);
            assert!(matches!(undo(&mut rig), Restored::Landed));
            set_trim(&mut rig, &top, 0.9);
            let after = hash(&rig, &top);
            assert!(matches!(redo(&mut rig), Restored::Nothing));
            assert_eq!(hash(&rig, &top), after);
        });
    }

    #[test]
    fn a_write_that_does_not_land_is_no_step() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("undo_rejected_write", "default.json");
            let voice = voice_source(&rig);
            let channel = rig.channel_source();
            let rejected = write(&mut rig.session, &channel, &voice, |store, expect| {
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
            assert!(!matches!(&rejected, Ok(result) if result.written));
            assert!(matches!(undo(&mut rig), Restored::Nothing));
        });
    }

    fn sidecar_describes(source: &str, description: &str) -> EditResult {
        let theirs = set_instrument_description(
            &SetInstrumentDescription {
                source: source.to_string(),
                description: Some(description.to_string()),
                expect: None,
            },
            &FsResolver::for_document(source).stat_only(),
        )
        .expect("the verb ran")
        .output;
        assert!(theirs.written, "{:?}", theirs.report);
        theirs
    }

    #[test]
    fn a_write_the_expect_guard_turns_away_is_no_step() {
        within(HANG_SECS, || {
            let (mut rig, top, _) = drums("undo_guarded_write");
            let theirs = sidecar_describes(&top, "the sidecar's");
            let channel = rig.channel_source();
            let ours = write(&mut rig.session, &channel, &top, |store, expect| {
                set_instrument_name(
                    &SetInstrumentName {
                        source: top.clone(),
                        name: "ours".to_string(),
                        expect,
                    },
                    store,
                )
            })
            .expect("the verb ran");
            assert!(!ours.written);
            assert_eq!(ours.hash, theirs.hash);
            assert!(matches!(undo(&mut rig), Restored::Nothing));
        });
    }

    #[test]
    fn a_sidecar_write_in_between_refuses_the_undo_and_keeps_every_step() {
        within(HANG_SECS, || {
            let (mut rig, top, strip) = drums("undo_conflict");
            set_trim(&mut rig, &top, 0.5);
            rename(&mut rig, &strip, "renamed-strip");
            let theirs = sidecar_describes(&strip, "the sidecar's");

            for _ in 0..2 {
                let refused = undo(&mut rig);
                assert!(
                    matches!(&refused, Restored::Conflict { step } if step.source == strip),
                    "{refused:?}"
                );
                assert_eq!(
                    hash(&rig, &strip),
                    theirs.hash,
                    "the sidecar's write stands"
                );
                assert!(rig.session.undo().note().is_some());
            }
        });
    }

    /// The editor writes, the sidecar writes over it, the editor writes again: the newest step
    /// still undoes onto the sidecar's state, and the conflict below it costs no redo.
    #[test]
    fn a_conflict_keeps_a_redo_that_still_lands() {
        within(HANG_SECS, || {
            let (mut rig, top, _) = drums("undo_conflict_redo");
            set_trim(&mut rig, &top, 0.5);
            let theirs = sidecar_describes(&top, "the sidecar's");
            rig.poll_until("the session reading the sidecar's write", |rig| {
                hash(rig, &top) == theirs.hash
            });
            set_trim(&mut rig, &top, 0.6);
            let ours = hash(&rig, &top);

            assert!(matches!(undo(&mut rig), Restored::Landed));
            assert_eq!(hash(&rig, &top), theirs.hash);
            assert!(matches!(undo(&mut rig), Restored::Conflict { .. }));
            assert_eq!(hash(&rig, &top), theirs.hash);

            assert!(matches!(redo(&mut rig), Restored::Landed));
            assert_eq!(hash(&rig, &top), ours);
        });
    }

    /// A subpatch gains an input, its parent wires a value into it, and the sidecar then writes
    /// the parent: undoing past that conflict must not take the input away from under the parent.
    #[test]
    fn undoing_past_a_conflict_never_strands_the_parent_on_a_subpatch_it_does_not_fit() {
        within(HANG_SECS, || {
            let (mut rig, top, strip) = drums("undo_conflict_parent");
            edit(&mut rig, &strip, |store, expect| {
                add_instrument_interface_input(
                    &AddInstrumentInterfaceInput {
                        source: strip.clone(),
                        name: "drive".to_string(),
                        type_name: "f32".to_string(),
                        value: Some(json!(0.0)),
                        expect,
                        ..Default::default()
                    },
                    store,
                )
            });
            let strip_with_drive = hash(&rig, &strip);
            edit(&mut rig, &top, |store, expect| {
                set_instrument_input(
                    &SetInstrumentInput {
                        source: top.clone(),
                        address: "/kick_strip".to_string(),
                        input: "drive".to_string(),
                        value: json!(0.5),
                        expect,
                    },
                    store,
                )
            });
            sidecar_describes(&top, "the sidecar's");

            assert!(matches!(undo(&mut rig), Restored::Conflict { .. }));
            assert!(matches!(undo(&mut rig), Restored::Conflict { .. }));
            assert_eq!(hash(&rig, &strip), strip_with_drive);
            let parent = rig.session.tree().top().read.as_ref().expect("reads");
            assert!(parent.report.ok, "{:?}", parent.report.errors);
            assert_eq!(rig.session.tree().blocked(), Vec::<String>::new());
        });
    }

    /// Four writes to a store that keeps four states: by the last undo, the state before the
    /// first write has been pushed out.
    #[test]
    fn a_state_evicted_from_the_store_drops_its_step_and_every_step_behind_it() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("undo_evicted", "default.json");
            rig.session.keep_states(4);
            let voice = voice_source(&rig);
            let mut states = vec![hash(&rig, &voice)];
            for cutoff in [900.0, 1000.0, 1100.0, 1200.0] {
                assert!(set_cutoff(&mut rig, &voice, cutoff).written);
                states.push(hash(&rig, &voice));
            }

            for state in states[1..4].iter().rev() {
                assert!(matches!(undo(&mut rig), Restored::Landed));
                assert_eq!(&hash(&rig, &voice), state);
            }
            let refused = undo(&mut rig);
            assert!(
                matches!(&refused, Restored::Forgotten { dropped: 1, .. }),
                "{refused:?}"
            );
            assert_eq!(hash(&rig, &voice), states[1], "nothing written");
            assert!(matches!(undo(&mut rig), Restored::Nothing));
            assert!(rig.session.undo().note().is_some());
            assert!(
                matches!(redo(&mut rig), Restored::Landed),
                "the redo stack is kept"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn a_restore_that_cannot_be_written_keeps_every_step() {
        use std::os::unix::fs::PermissionsExt;

        within(HANG_SECS, || {
            let (mut rig, top, strip) = drums("undo_unwritable");
            set_trim(&mut rig, &top, 0.5);
            let original = hash(&rig, &strip);
            rename(&mut rig, &strip, "renamed-strip");
            let renamed = hash(&rig, &strip);
            let dir = rig
                .session
                .tree()
                .document(&strip)
                .unwrap()
                .path
                .parent()
                .unwrap()
                .to_path_buf();
            let mode = |bits| std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(bits));

            mode(0o555).unwrap();
            let refused = undo(&mut rig);
            mode(0o755).unwrap();
            assert!(matches!(&refused, Restored::Refused { .. }), "{refused:?}");
            assert_eq!(hash(&rig, &strip), renamed);
            assert!(rig.session.undo().note().is_some());

            assert!(matches!(undo(&mut rig), Restored::Landed));
            assert_eq!(hash(&rig, &strip), original);
            assert!(
                matches!(undo(&mut rig), Restored::Landed),
                "the step behind it is kept"
            );
        });
    }

    #[test]
    fn a_state_that_no_longer_validates_is_reported_and_its_step_stays() {
        within(HANG_SECS, || {
            let (mut rig, top, strip) = drums("undo_invalid");
            rename(&mut rig, &top, "renamed-drums");
            let renamed = hash(&rig, &top);

            // Every strip of the top-level file wires `filter`; the subpatch stops offering it.
            let theirs = remove_instrument_interface_input(
                &RemoveInstrumentInterfacePipe {
                    source: strip.clone(),
                    name: "filter".to_string(),
                    expect: None,
                },
                &FsResolver::for_document(&strip).stat_only(),
            )
            .expect("the verb ran")
            .output;
            assert!(theirs.written, "{:?}", theirs.report);

            let refused = undo(&mut rig);
            let Restored::Rejected { step, result } = &refused else {
                panic!("{refused:?}");
            };
            assert!(!result.report.errors.is_empty());
            assert_eq!(step.source, top);
            assert_eq!(hash(&rig, &top), renamed, "nothing written");
            assert_eq!(rig.session.undo().next(Direction::Undo), Some(step));
            let note = rig.session.undo().note().expect("a note");
            assert!(note.contains(&result.report.errors[0].message), "{note}");
        });
    }

    #[test]
    fn a_file_that_does_not_read_keeps_its_step_until_it_is_repaired() {
        within(HANG_SECS, || {
            let (mut rig, _, strip) = drums("undo_unreadable");
            let original = hash(&rig, &strip);
            rename(&mut rig, &strip, "renamed-strip");
            let path = rig.session.tree().document(&strip).unwrap().path.clone();
            let good = std::fs::read_to_string(&path).unwrap();

            std::fs::write(&path, "{ not an instrument").unwrap();
            let refused = undo(&mut rig);
            assert!(matches!(&refused, Restored::Refused { .. }), "{refused:?}");
            assert!(rig.session.undo().next(Direction::Undo).is_some());

            std::fs::write(&path, good).unwrap();
            assert!(matches!(undo(&mut rig), Restored::Landed));
            assert_eq!(hash(&rig, &strip), original);
        });
    }
}
