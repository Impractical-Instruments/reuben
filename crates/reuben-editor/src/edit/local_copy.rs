//! "Make a local copy": give the document being viewed a file of its own for this instrument, so
//! an edit to it stops reaching every other document that references the original.
//!
//! Two writes, each a `reuben-api` verb: `copy_instrument` lays the copy beside the original, and
//! `set_instrument_resource` repoints the referencing document's `resources` entry at it. Only the
//! repoint is an undo step. Undoing it puts the entry back on the original; the copy stays on disk,
//! unreferenced, and a redo points the entry at it again.

use std::path::Path;

use reuben_api::authoring::{CopyInstrument, EditResult, Refusal, SetInstrumentResource};
use reuben_api::engine::{Channel, ListWhat};
use reuben_api::tools::names;

use super::{call, write, Verb};
use crate::feedback::Feedback;
use crate::names::lexical;
use crate::session::Session;

/// Copy the viewed document beside itself, repoint the document one step up the trail at the
/// copy, and go on viewing the copy. Returns the copy's source when both writes landed.
///
/// What did not land is the session's [`Feedback`], as for any edit. A copy whose repoint did not
/// land stays on disk, unreferenced, as an undone local copy's does.
pub fn make(session: &mut Session, channel: &dyn Fn() -> Option<Channel>) -> Option<String> {
    let Some(crumb) = session.trail().last().cloned() else {
        refuse(session, "", "the top-level document is no copy of anything");
        return None;
    };
    let depth = session.trail().len() - 1;
    let parent = match depth {
        0 => Some(session.tree().top()),
        _ => session.tree().document(&session.trail()[depth - 1].source),
    };
    let Some(parent) = parent.filter(|parent| parent.read.is_ok()) else {
        refuse(
            session,
            &crumb.source,
            "the document above it does not read",
        );
        return None;
    };
    let Some(entry) = parent.reference(&crumb.address).cloned() else {
        let why = format!("{} no longer references it", crumb.address);
        refuse(session, &crumb.source, &why);
        return None;
    };
    let parent_source = parent.source.clone();
    let Some(server) = channel() else {
        refuse(
            session,
            &crumb.source,
            "no engine is running to name a copy",
        );
        return None;
    };
    let offered = match server.list(ListWhat::Instruments) {
        Ok(offered) => offered,
        Err(e) => {
            let why = format!("the server could not list its instruments: {e}");
            refuse(session, &crumb.source, &why);
            return None;
        }
    };
    let taken = shadowing(&crumb.source, &parent_source, &entry.source, offered);
    let owner = parent_stem(&parent_source);
    let names = free_names(&crumb.source, &owner, &taken);
    let copied = copy_to_free(names, |to| {
        let verb = CopyInstrument {
            from: crumb.source.clone(),
            to: to.to_string(),
        };
        call(channel, Verb::of(names::COPY_INSTRUMENT, &verb))
    });
    let Some((to, copied)) = copied else {
        refuse(session, &crumb.source, "no free name beside it for a copy");
        return None;
    };
    if let Some(feedback) = Feedback::of(&to, None, &copied) {
        *session.feedback_mut() = Some(feedback);
        return None;
    }
    let copy_file = Path::new(&to).file_name().unwrap_or_default();
    let repointed = renamed(&entry.source, &copy_file.to_string_lossy());

    let landed = write(session, channel, &parent_source, |expect| {
        Verb::of(
            names::SET_INSTRUMENT_RESOURCE,
            &SetInstrumentResource {
                source: parent_source.clone(),
                id: entry.id,
                resource_source: repointed,
                expect,
            },
        )
    })
    .is_ok_and(|result| result.written);
    if !landed {
        return None;
    }
    // The repoint moved the node off the document the trail named, so the reload let the trail go
    // back up to the referencing document. Down again, now to the copy.
    if session.trail().len() == depth {
        // A copy that does not read is not entered; the session shows the tree as it is.
        let _ = session.enter(&crumb.address);
    }
    Some(to)
}

fn refuse(session: &mut Session, source: &str, why: &str) {
    let refused: Result<EditResult, Refusal> = Err(Refusal {
        message: format!("cannot make a local copy: {why}"),
    });
    *session.feedback_mut() = Feedback::of(source, None, &refused);
}

/// How many names a local copy tries before it gives up: enough to step past every copy a person
/// would make by hand; past it something is wrong.
const MOST_NAMES: usize = 100;

/// The referencing document's file stem, which names whose copy it is.
fn parent_stem(parent: &str) -> String {
    (Path::new(parent).file_stem())
        .map_or_else(|| "local".to_string(), |s| s.to_string_lossy().into_owned())
}

/// `<stem>-<owner>.json`, `<stem>-<owner>-2.json`, … beside `source`, as many as
/// [`MOST_NAMES`], leaving out each that is one of `offered`.
pub fn free_names<'a>(
    source: &str,
    owner: &str,
    offered: &'a [String],
) -> impl Iterator<Item = String> + 'a {
    let path = Path::new(source);
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    let extension =
        (path.extension()).map_or_else(String::new, |e| format!(".{}", e.to_string_lossy()));
    let beside = path.to_path_buf();
    let base = format!("{stem}-{owner}");
    (1..=MOST_NAMES)
        .map(move |n| match n {
            1 => format!("{base}{extension}"),
            n => format!("{base}-{n}{extension}"),
        })
        .map(move |name| beside.with_file_name(name).display().to_string())
        .filter(move |name| !offered.contains(name))
}

/// Copy through `copy` to the first of `names` the server takes, stepping past each it refuses as
/// already holding a document: a name the server's list did not show, spelled another way or in a
/// directory it does not list, is only found taken when the copy is refused. Returns the name
/// last tried and what the copy to it answered; `None` when every name was refused so.
fn copy_to_free<T>(
    names: impl Iterator<Item = String>,
    mut copy: impl FnMut(&str) -> Result<T, Refusal>,
) -> Option<(String, Result<T, Refusal>)> {
    for to in names {
        let copied = copy(&to);
        match &copied {
            Err(refusal) if refusal.message.contains("a document already exists") => continue,
            _ => return Some((to, copied)),
        }
    }
    None
}

/// `offered`, with every name beside `source` whose reference from `parent` would be found first
/// somewhere else: a reference resolves from the referencing document's directory, so a copy
/// named `x` beside `source` is only reached through `reference` when nothing named `x` sits
/// where `reference`'s directory lands from `parent`.
fn shadowing(source: &str, parent: &str, reference: &str, mut offered: Vec<String>) -> Vec<String> {
    let beside = Path::new(source).parent().map(lexical);
    let reached = Path::new(parent)
        .parent()
        .map(|dir| lexical(&dir.join(reference)))
        .and_then(|path| path.parent().map(Path::to_path_buf));
    if let (Some(beside), Some(reached)) = (beside, reached) {
        if beside != reached {
            let shadows: Vec<String> = (offered.iter())
                .filter(|name| Path::new(name.as_str()).parent() == Some(reached.as_path()))
                .filter_map(|name| Path::new(name).file_name())
                .map(|file| beside.join(file).display().to_string())
                .collect();
            offered.extend(shadows);
        }
    }
    offered
}

/// `reference` with its last path segment replaced by `name`, every other segment kept as the
/// document spells it.
fn renamed(reference: &str, name: &str) -> String {
    match reference.rfind(std::path::is_separator) {
        Some(at) => format!("{}{name}", &reference[..=at]),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use reuben_api::authoring::{set_instrument_description, SetInstrumentDescription};
    use reuben_native::test_support::within;

    use super::*;
    use crate::edit::restore;
    use crate::session::test_support::Rig;
    use crate::session::{Direction, Status};

    const HANG_SECS: u64 = 60;

    fn hash_on_disk(path: &Path) -> String {
        let source = path.display().to_string();
        reuben_api::authoring::read_instrument(
            &reuben_api::authoring::ReadInstrument {
                source: source.clone(),
            },
            &reuben_api::FsResolver::for_document(&source).stat_only(),
        )
        .unwrap_or_else(|e| panic!("{source} reads: {e}"))
        .output
        .hash
    }

    fn kick_entry(rig: &Rig, top: &Path) -> String {
        let document = (rig.session.tree().documents().iter())
            .find(|d| crate::session::test_support::names(&d.source, top))
            .expect("the top-level document is in the tree");
        let structure = document.read.as_ref().expect("it reads");
        (structure.resources.iter())
            .find(|entry| entry.id == "kick-voice")
            .expect("the kick voice entry")
            .source
            .clone()
    }

    #[test]
    fn a_reference_is_renamed_in_its_last_segment_only() {
        assert_eq!(renamed("voices/kick.json", "k.json"), "voices/k.json");
        assert_eq!(renamed("kick.json", "k.json"), "k.json");
        assert_eq!(renamed("../a/b/kick.json", "k.json"), "../a/b/k.json");
    }

    #[test]
    fn the_first_free_name_is_taken() {
        let source = "/l/voices/kick-voice.json";
        let first = |offered: &[String]| free_names(source, "acid", offered).next();
        assert_eq!(
            first(&[]).as_deref(),
            Some("/l/voices/kick-voice-acid.json")
        );
        let offered = [
            "/l/voices/kick-voice.json".to_string(),
            "/l/voices/kick-voice-acid.json".to_string(),
            "/l/kick-voice-acid-2.json".to_string(),
        ];
        assert_eq!(
            first(&offered).as_deref(),
            Some("/l/voices/kick-voice-acid-2.json"),
            "a name elsewhere is not beside the source"
        );
        let all: Vec<String> = free_names(source, "acid", &[]).collect();
        assert_eq!(all.len(), MOST_NAMES);
        assert_eq!(first(&all), None);
    }

    #[test]
    fn a_name_the_server_refuses_as_taken_is_stepped_past() {
        let names = free_names("/l/voices/kick-voice.json", "acid", &[]);
        let mut tried = Vec::new();
        let (to, copied) = copy_to_free(names, |to| {
            tried.push(to.to_string());
            match tried.len() {
                1 => Err(Refusal {
                    message: format!(
                        "a document already exists at `{to}`; choose a new source for the copy"
                    ),
                }),
                _ => Ok(()),
            }
        })
        .expect("a name the server takes");
        assert_eq!(to, "/l/voices/kick-voice-acid-2.json");
        assert!(copied.is_ok());
        assert_eq!(tried.len(), 2);

        let other = Refusal {
            message: "no engine is running".to_string(),
        };
        let names = free_names("/l/voices/kick-voice.json", "acid", &[]);
        let (to, copied) = copy_to_free(names, |_| Err::<(), _>(other.clone())).expect("an answer");
        assert_eq!(
            to, "/l/voices/kick-voice-acid.json",
            "any other refusal stands"
        );
        assert!(copied.is_err());

        let names = free_names("/l/voices/kick-voice.json", "acid", &[]);
        let taken = |to: &str| {
            Err::<(), _>(Refusal {
                message: format!("a document already exists at `{to}`"),
            })
        };
        assert!(copy_to_free(names, taken).is_none(), "every name refused");
    }

    /// The refusal [`copy_to_free`] steps past is the one the server answers a copy onto a
    /// document with.
    #[test]
    fn the_server_refuses_a_copy_onto_a_document_as_taken() {
        within(HANG_SECS, || {
            let rig = Rig::open("local_copy_taken", "acid-techno.json");
            let voices = rig.lib.join("voices");
            let verb = CopyInstrument {
                from: voices.join("kick-voice.json").display().to_string(),
                to: voices.join("shaped-vca.json").display().to_string(),
            };
            let channel = rig.channel_source();
            let refused = call(&channel, Verb::of(names::COPY_INSTRUMENT, &verb))
                .expect_err("the copy is refused");
            assert!(
                refused.message.contains("a document already exists"),
                "{}",
                refused.message
            );
        });
    }

    #[test]
    fn a_name_the_reference_would_find_first_elsewhere_is_taken() {
        let offered = vec!["/top/voices/kick-voice-mine.json".to_string()];
        let taken = shadowing(
            "/lib/voices/kick-voice.json",
            "/top/mine.json",
            "voices/kick-voice.json",
            offered,
        );
        assert_eq!(
            free_names("/lib/voices/kick-voice.json", "mine", &taken).next(),
            Some("/lib/voices/kick-voice-mine-2.json".to_string())
        );
        let beside = shadowing(
            "/lib/voices/kick-voice.json",
            "/lib/acid.json",
            "./voices/../voices/kick-voice.json",
            Vec::new(),
        );
        assert!(beside.is_empty(), "{beside:?}");
    }

    /// The issue's acceptance: after a local copy, editing the voice changes this instrument only.
    #[test]
    fn a_local_copy_takes_the_edits_and_leaves_the_shared_original_alone() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("local_copy", "acid-techno.json");
            rig.session
                .enter("/kick_v")
                .expect("/kick_v voices kick-voice");
            let original = rig.lib.join("voices").join("kick-voice.json");
            let original_hash = hash_on_disk(&original);
            let others = ["euclidean-drums.json", "groovebox.json"]
                .map(|name| (name, hash_on_disk(&rig.lib.join(name))));

            let channel = rig.channel_source();
            let copy = make(&mut rig.session, &channel).expect("the copy is made");
            assert_eq!(rig.session.feedback(), None);
            let copy_path = rig.lib.join("voices").join("kick-voice-acid-techno.json");
            assert_eq!(copy, copy_path.display().to_string());
            assert!(
                crate::session::test_support::names(&rig.session.viewed().source, &copy_path),
                "the view is on the copy"
            );
            assert_eq!(rig.session.trail().len(), 1);
            assert_eq!(
                kick_entry(&rig, &rig.top.clone()),
                "voices/kick-voice-acid-techno.json"
            );
            assert_eq!(hash_on_disk(&copy_path), original_hash);

            let source = rig.session.viewed().source.clone();
            let edited = write(&mut rig.session, &channel, &source, |expect| {
                Verb::of(
                    names::SET_INSTRUMENT_DESCRIPTION,
                    &SetInstrumentDescription {
                        source: source.clone(),
                        description: Some("acid-techno's own kick".to_string()),
                        expect,
                    },
                )
            })
            .expect("the verb ran");
            assert!(edited.written, "{:?}", edited.report);

            assert_eq!(hash_on_disk(&original), original_hash);
            assert_ne!(hash_on_disk(&copy_path), original_hash);
            for (name, hash) in &others {
                assert_eq!(&hash_on_disk(&rig.lib.join(name)), hash, "{name}");
            }
            rig.settle();
            assert_eq!(rig.status(), &Status::Current);
        });
    }

    /// Undo takes back the repoint, not the copy: the entry names the original again and the copy
    /// is left on disk. Redo points it at the copy again.
    #[test]
    fn undo_puts_the_entry_back_and_leaves_the_copy_on_disk() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("local_copy_undo", "acid-techno.json");
            rig.session
                .enter("/kick_v")
                .expect("/kick_v voices kick-voice");
            let channel = rig.channel_source();
            let copy = make(&mut rig.session, &channel).expect("the copy is made");
            let top = rig.top.clone();

            restore(&mut rig.session, &channel, Direction::Undo);
            assert_eq!(kick_entry(&rig, &top), "voices/kick-voice.json");
            assert!(Path::new(&copy).exists(), "nothing deletes the copy");
            assert!(rig.session.trail().is_empty(), "the view goes back up");

            restore(&mut rig.session, &channel, Direction::Redo);
            assert_eq!(kick_entry(&rig, &top), "voices/kick-voice-acid-techno.json");
            assert!(
                rig.session.trail().is_empty(),
                "redo leaves the view where it is"
            );
        });
    }

    /// A local copy made again after an undo does not overwrite the first, which the redo stack
    /// may still point at.
    #[test]
    fn a_name_already_taken_is_stepped_past() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("local_copy_again", "acid-techno.json");
            rig.session
                .enter("/kick_v")
                .expect("/kick_v voices kick-voice");
            let channel = rig.channel_source();
            let first = make(&mut rig.session, &channel).expect("the first copy");
            restore(&mut rig.session, &channel, Direction::Undo);
            rig.session
                .enter("/kick_v")
                .expect("/kick_v voices kick-voice again");
            let second = make(&mut rig.session, &channel).expect("the second copy");
            assert_ne!(first, second);
            let second_path = rig.lib.join("voices").join("kick-voice-acid-techno-2.json");
            assert_eq!(second, second_path.display().to_string());
            assert!(crate::session::test_support::names(
                &rig.session.viewed().source,
                &second_path
            ));
            assert_eq!(
                kick_entry(&rig, &rig.top.clone()),
                "voices/kick-voice-acid-techno-2.json"
            );
        });
    }

    /// Two steps down, the document one step up is itself a voice: the repoint is written there,
    /// and the copy is named for it.
    #[test]
    fn a_copy_two_steps_down_repoints_the_voice_above_it() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("local_copy_deep", "acid-techno.json");
            rig.session
                .enter("/kick_v")
                .expect("/kick_v voices kick-voice");
            rig.session.enter("/vca").expect("/vca patches shaped-vca");
            let voices = rig.lib.join("voices");
            let original = voices.join("shaped-vca.json");
            let original_hash = hash_on_disk(&original);
            let top_hash = hash_on_disk(&rig.top.clone());

            let channel = rig.channel_source();
            let copy = make(&mut rig.session, &channel).expect("the copy is made");
            let copy_path = voices.join("shaped-vca-kick-voice.json");
            assert_eq!(copy, copy_path.display().to_string());
            assert!(crate::session::test_support::names(
                &rig.session.viewed().source,
                &copy_path
            ));
            assert_eq!(rig.session.trail().len(), 2);
            let kick = (rig.session.tree().documents().iter())
                .find(|d| {
                    crate::session::test_support::names(&d.source, &voices.join("kick-voice.json"))
                })
                .expect("the kick voice is in the tree");
            let entry = kick.reference("/vca").expect("/vca references a patch");
            assert_eq!(entry.source, "shaped-vca-kick-voice.json");
            assert_eq!(hash_on_disk(&original), original_hash);
            assert_eq!(hash_on_disk(&rig.top.clone()), top_hash);
        });
    }

    /// A copy whose repoint loses to a write made since the session read the document above is
    /// left on disk, unreferenced; the conflict is the session's feedback, the view stays on the
    /// original, and there is nothing to undo.
    #[test]
    fn a_repoint_that_conflicts_leaves_the_copy_unreferenced_and_no_undo_step() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("local_copy_conflict", "acid-techno.json");
            rig.session
                .enter("/kick_v")
                .expect("/kick_v voices kick-voice");
            let top = rig.top.display().to_string();
            let elsewhere = set_instrument_description(
                &SetInstrumentDescription {
                    source: top.clone(),
                    description: Some("written by someone else".to_string()),
                    expect: None,
                },
                &reuben_api::FsResolver::for_document(&top).stat_only(),
            )
            .expect("the verb ran")
            .output;
            assert!(elsewhere.written, "{:?}", elsewhere.report);

            let channel = rig.channel_source();
            assert_eq!(make(&mut rig.session, &channel), None);
            assert!(
                matches!(rig.session.feedback(), Some(Feedback::Conflict { .. })),
                "{:?}",
                rig.session.feedback()
            );
            let copy_path = rig.lib.join("voices").join("kick-voice-acid-techno.json");
            assert!(copy_path.exists(), "the copy is left on disk");
            assert_eq!(kick_entry(&rig, &rig.top.clone()), "voices/kick-voice.json");
            assert!(crate::session::test_support::names(
                &rig.session.viewed().source,
                &rig.lib.join("voices").join("kick-voice.json")
            ));
            assert_eq!(rig.session.undo().next(Direction::Undo), None);
        });
    }

    #[test]
    fn the_top_level_document_has_nothing_to_copy_and_says_so_through_the_feedback() {
        within(HANG_SECS, || {
            let mut rig = Rig::open("local_copy_top", "acid-techno.json");
            let channel = rig.channel_source();
            assert_eq!(make(&mut rig.session, &channel), None);
            assert!(
                matches!(rig.session.feedback(), Some(Feedback::Refused { .. })),
                "{:?}",
                rig.session.feedback()
            );
        });
    }
}
