//! "Make a local copy": give the document being viewed a file of its own for this instrument, so
//! an edit to it stops reaching every other document that references the original.
//!
//! Two writes, each a `reuben-api` verb: `copy_instrument` lays the copy beside the original, and
//! `set_instrument_resource` repoints the referencing document's `resources` entry at it. Only the
//! repoint is an undo step. Undoing it puts the entry back on the original; the copy stays on disk,
//! unreferenced, and a redo points the entry at it again.

use std::path::{Path, PathBuf};

use reuben_api::authoring::{CopyInstrument, EditResult, Refusal, SetInstrumentResource};
use reuben_api::engine::Channel;
use reuben_api::tools::names;

use super::{call, write, Verb};
use crate::feedback::Feedback;
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
    let from = PathBuf::from(&crumb.source);
    let beside_parent = parent.path.parent().map(Path::to_path_buf);
    let owner = parent_stem(&parent.path);
    let Some((to, repointed)) = free_name(&from, &owner, |name| {
        let reference = renamed(&entry.source, name);
        // The reference must land on the copy: a file of that name beside the referencing
        // document would be found before the one beside the original.
        let taken = from.with_file_name(name).exists()
            || (beside_parent.as_ref()).is_some_and(|dir| dir.join(&reference).exists());
        (!taken).then_some(reference)
    }) else {
        refuse(session, &crumb.source, "no free name beside it for a copy");
        return None;
    };

    let to = to.display().to_string();
    let copied = call(
        channel,
        Verb::of(
            names::COPY_INSTRUMENT,
            &CopyInstrument {
                from: crumb.source.clone(),
                to: to.clone(),
            },
        ),
    );
    if let Some(feedback) = Feedback::of(&to, None, &copied) {
        *session.feedback_mut() = Some(feedback);
        return None;
    }

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

/// The referencing document's file stem, which names whose copy it is.
fn parent_stem(parent: &Path) -> String {
    parent
        .file_stem()
        .map_or_else(|| "local".to_string(), |s| s.to_string_lossy().into_owned())
}

/// The first of `<stem>-<owner>.json`, `<stem>-<owner>-2.json`, … that `free` accepts, beside
/// `from`, with what `free` answered for it.
fn free_name<T>(
    from: &Path,
    owner: &str,
    mut free: impl FnMut(&str) -> Option<T>,
) -> Option<(PathBuf, T)> {
    let stem = from.file_stem()?.to_string_lossy();
    let extension = from
        .extension()
        .map_or_else(String::new, |e| format!(".{}", e.to_string_lossy()));
    // Enough to step past every copy a person would make by hand; past it something is wrong.
    for n in 1..=100 {
        let suffix = if n == 1 {
            String::new()
        } else {
            format!("-{n}")
        };
        let name = format!("{stem}-{owner}{suffix}{extension}");
        if let Some(answer) = free(&name) {
            return Some((from.with_file_name(&name), answer));
        }
    }
    None
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
            .find(|d| d.path == top)
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
        let from = Path::new("voices").join("kick-voice.json");
        let (path, n) = free_name(&from, "acid", |name| {
            (name != "kick-voice-acid.json").then(|| name.to_string())
        })
        .expect("a free name");
        assert_eq!(path, Path::new("voices").join("kick-voice-acid-2.json"));
        assert_eq!(n, "kick-voice-acid-2.json");
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
            assert_eq!(
                rig.session.viewed().path,
                copy_path,
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
            rig.poll_until("the Swap of the edits", |rig| {
                rig.status() == &Status::Current && rig.swaps() >= 1
            });
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
            assert_eq!(rig.session.viewed().path, second_path);
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
            assert_eq!(rig.session.viewed().path, copy_path);
            assert_eq!(rig.session.trail().len(), 2);
            let kick = (rig.session.tree().documents().iter())
                .find(|d| d.path == voices.join("kick-voice.json"))
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
            assert_eq!(
                rig.session.viewed().path,
                rig.lib.join("voices").join("kick-voice.json")
            );
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
