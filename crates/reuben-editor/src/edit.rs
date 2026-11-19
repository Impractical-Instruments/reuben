//! The editor's one write path: every change to a document is a `reuben-api` authoring verb called
//! through [`write`], and every return to an earlier state is a restore through [`restore`].

use std::path::Path;

use reuben_api::authoring::{
    read_instrument, restore_instrument, Answer, EditResult, ReadInstrument, Refusal, Resources,
};
use reuben_api::engine::Channel;

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
/// conflict; its report says which, on the node and port at fault.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "no gesture edits a document yet")
)]
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
    let result = verb(session.history(source), expect.clone()).map(|answer| answer.output);
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
    /// left it. None of that file's steps can land on what it holds now, so `dropped` of them,
    /// this one included, are gone from both stacks.
    Conflict { step: Step, dropped: usize },
    /// The state the step names no longer validates, a file it references having changed since.
    /// Nothing was written, and the step stays: it lands again once that file is put back.
    Rejected { step: Step, result: EditResult },
    /// The state the step names is no longer held. It, and every step behind it in the same
    /// direction — `dropped` in all — are gone.
    Forgotten { step: Step, dropped: usize },
    /// The file does not read as a document, so it has no hash a restore could be guarded by.
    /// The step stays, to land once the file is repaired.
    Unreadable { step: Step, why: String },
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
            Restored::Conflict { step, dropped, .. } => Some(format!(
                "Cannot {verb}: {} was changed outside the editor since. It is left as it is, and \
                 its {dropped} undo/redo step(s) are dropped.",
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
            Restored::Unreadable { step, why } => Some(format!(
                "Cannot {verb}: {} does not read as an instrument ({why}). Repair it and try again.",
                file_name(&step.source)
            )),
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
/// The session reads the files again before this returns, as after [`write`], and holds the
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
            Restored::Landed
        }
        Ok(answer) if answer.output.hash != expect => conflict(session, step),
        Ok(answer) => Restored::Rejected {
            step,
            result: answer.output,
        },
        // A refusal is a state no longer held or a file that is not a document; which, and
        // whether someone else's write is the real news, takes a read of the file.
        Err(refusal) => {
            let read = read_instrument(
                &ReadInstrument {
                    source: step.source.clone(),
                },
                session.history(&step.source),
            );
            match read {
                Err(_) => Restored::Unreadable {
                    step,
                    why: refusal.message,
                },
                Ok(read) if read.output.hash != expect => conflict(session, step),
                Ok(_) => {
                    let dropped = session.undo_mut().forget(direction);
                    Restored::Forgotten { step, dropped }
                }
            }
        }
    };
    let note = restored.note(direction);
    session.undo_mut().set_note(note);
    session.refresh(channel);
    restored
}

fn conflict(session: &mut Session, step: Step) -> Restored {
    let dropped = session.undo_mut().forget_file(&step.source);
    Restored::Conflict { step, dropped }
}

#[cfg(test)]
mod tests {
    use reuben_api::authoring::{
        remove_instrument_interface_input, set_instrument_description, set_instrument_input,
        set_instrument_name, RemoveInstrumentInterfacePipe, SetInstrumentDescription,
        SetInstrumentInput, SetInstrumentName,
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

    #[test]
    fn a_sidecar_write_in_between_makes_the_undo_refuse_and_is_left_as_it_is() {
        within(HANG_SECS, || {
            let (mut rig, top, strip) = drums("undo_conflict");
            let original_top = hash(&rig, &top);
            set_trim(&mut rig, &top, 0.5);
            rename(&mut rig, &strip, "renamed-strip");
            rename(&mut rig, &strip, "renamed-again");

            let theirs = set_instrument_description(
                &SetInstrumentDescription {
                    source: strip.clone(),
                    description: Some("the sidecar's".to_string()),
                    expect: None,
                },
                &FsResolver::for_document(&strip).stat_only(),
            )
            .expect("the verb ran")
            .output;
            assert!(theirs.written);

            let refused = undo(&mut rig);
            assert!(
                matches!(&refused, Restored::Conflict { step, dropped: 2 } if step.source == strip),
                "{refused:?}"
            );
            assert_eq!(
                hash(&rig, &strip),
                theirs.hash,
                "the sidecar's write stands"
            );
            assert!(rig.session.undo().note().is_some());

            // The steps of the file nobody else wrote are still there.
            assert!(matches!(undo(&mut rig), Restored::Landed));
            assert_eq!(hash(&rig, &top), original_top);
            assert_eq!(hash(&rig, &strip), theirs.hash);
            assert!(matches!(undo(&mut rig), Restored::Nothing));
        });
    }

    #[test]
    fn a_state_no_longer_held_drops_its_step_and_every_step_behind_it() {
        within(HANG_SECS, || {
            let (mut rig, top, _) = drums("undo_forgotten");
            set_trim(&mut rig, &top, 0.5);
            let now = hash(&rig, &top);
            rig.session.undo_mut().record(Step {
                source: top.clone(),
                before: "0000000000000000".to_string(),
                after: now.clone(),
            });

            let refused = undo(&mut rig);
            assert!(
                matches!(&refused, Restored::Forgotten { dropped: 2, .. }),
                "{refused:?}"
            );
            assert_eq!(hash(&rig, &top), now, "nothing written");
            assert!(matches!(undo(&mut rig), Restored::Nothing));
            assert!(rig.session.undo().note().is_some());
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
            assert!(
                matches!(&refused, Restored::Unreadable { .. }),
                "{refused:?}"
            );
            assert!(rig.session.undo().next(Direction::Undo).is_some());

            std::fs::write(&path, good).unwrap();
            assert!(matches!(undo(&mut rig), Restored::Landed));
            assert_eq!(hash(&rig, &strip), original);
        });
    }
}
