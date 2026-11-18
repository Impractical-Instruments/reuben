//! Integration: `copy_instrument` over the real filesystem resolver, on a shipped voice that nests
//! a sibling — so "beside the original" and "another directory" are directories on a disk rather
//! than a test store's idea of one. Every case works on a private copy of the fixtures under the
//! system temp directory; nothing is written into `instruments/`.

use std::path::{Path, PathBuf};

use reuben_api::authoring::{self, CopyInstrument, ValidateInstrument};
use reuben_api::FsResolver;

/// A private `<temp>/<name>/voices/` holding `kick-voice.json` and the `shaped-vca.json` it nests,
/// removed on drop so a failing assertion leaves nothing behind.
struct Voices {
    root: PathBuf,
    dir: PathBuf,
}

impl Voices {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("reuben_copy_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("voices");
        std::fs::create_dir_all(&dir).unwrap();
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instruments/voices");
        for file in ["kick-voice.json", "shaped-vca.json"] {
            std::fs::copy(shipped.join(file), dir.join(file)).unwrap();
        }
        Self { root, dir }
    }
}

impl Drop for Voices {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl std::ops::Deref for Voices {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.dir
    }
}

fn voices(name: &str) -> Voices {
    Voices::new(name)
}

/// Copy as a host does: one resolver scoped to the document being copied.
fn copy(
    from: &Path,
    to: &Path,
) -> Result<authoring::Answer<authoring::EditResult>, authoring::Refusal> {
    let resolver = FsResolver::for_document(&from.display().to_string());
    copy_through(from, to, &resolver)
}

fn copy_through(
    from: &Path,
    to: &Path,
    resolver: &FsResolver,
) -> Result<authoring::Answer<authoring::EditResult>, authoring::Refusal> {
    authoring::copy_instrument(
        &CopyInstrument {
            from: from.display().to_string(),
            to: to.display().to_string(),
        },
        resolver,
    )
}

fn hash(path: &Path) -> String {
    let source = path.display().to_string();
    let json = std::fs::read_to_string(path).unwrap();
    authoring::hash_instrument(&json, &FsResolver::for_document(&source)).unwrap()
}

#[test]
fn a_copy_beside_the_original_is_written_valid_and_hashes_equal() {
    let dir = voices("beside");
    let (from, to) = (dir.join("kick-voice.json"), dir.join("kick-copy.json"));
    let original = std::fs::read_to_string(&from).unwrap();

    let result = copy(&from, &to).expect("a copy to a free sibling").output;

    assert!(result.written && result.report.ok, "{result:?}");
    assert!(result.report.warnings.is_empty(), "{result:?}");
    assert_eq!(result.hash, hash(&from));
    assert_eq!(result.hash, hash(&to));
    assert_eq!(std::fs::read_to_string(&from).unwrap(), original);

    // Read back by the copy's own name, as the next verb on it will.
    let source = to.display().to_string();
    let report = authoring::validate_instrument(
        &ValidateInstrument {
            source: source.clone(),
        },
        &FsResolver::for_document(&source),
    )
    .unwrap()
    .output;
    assert!(report.ok && report.warnings.is_empty(), "{report:?}");
}

/// A bare name is a sibling of `from` for a resolver scoped to `from`, so it lands beside it.
#[test]
fn a_bare_destination_name_lands_beside_the_original() {
    let dir = voices("bare");
    let from = dir.join("kick-voice.json");

    let result = copy(&from, Path::new("kick-copy.json"))
        .expect("a sibling by name")
        .output;

    assert!(result.written, "{result:?}");
    assert_eq!(result.hash, hash(&dir.join("kick-copy.json")));
}

#[test]
fn an_existing_destination_refuses_and_is_untouched() {
    let dir = voices("existing");
    let (from, to) = (dir.join("kick-voice.json"), dir.join("shaped-vca.json"));
    let taken = std::fs::read_to_string(&to).unwrap();

    let refusal = copy(&from, &to).expect_err("no overwrite");

    assert!(refusal.message.contains("already exists"), "{refusal}");
    assert_eq!(std::fs::read_to_string(&to).unwrap(), taken);
    // Copying a document onto itself is the same refusal.
    copy(&from, &from).expect_err("no overwrite of the original either");
}

#[test]
fn a_destination_in_another_directory_refuses() {
    let dir = voices("elsewhere");
    let from = dir.join("kick-voice.json");

    for to in [
        dir.parent().unwrap().join("kick-copy.json"),
        dir.join("deeper/kick-copy.json"),
    ] {
        let refusal = copy(&from, &to).expect_err("its references would not resolve there");
        assert!(refusal.message.contains("same directory"), "{refusal}");
        assert!(!to.exists(), "{} was written", to.display());
    }
    assert!(
        !dir.join("deeper").exists(),
        "a refused copy made a directory"
    );
    // The same directory under another spelling is still the same directory.
    let respelled = dir.join("deeper/../kick-copy.json");
    assert!(copy(&from, &respelled).expect("beside it").output.written);
    assert!(dir.join("kick-copy.json").is_file());
    assert!(
        !dir.join("deeper").exists(),
        "the copy made a directory it did not land in"
    );
}

#[test]
fn a_source_that_fails_validation_writes_nothing() {
    let dir = voices("invalid");
    let (from, to) = (dir.join("broken.json"), dir.join("broken-copy.json"));
    let kick = std::fs::read_to_string(dir.join("kick-voice.json")).unwrap();
    std::fs::write(
        &from,
        kick.replace("\"type\": \"subpatch\"", "\"type\": \"nosuchoperator\""),
    )
    .unwrap();

    let result = copy(&from, &to)
        .expect("the copy ran; its report says no")
        .output;
    assert!(!result.written && !result.report.ok, "{result:?}");
    assert!(!to.exists());

    std::fs::write(&from, "{ not a document").unwrap();
    copy(&from, &to).expect_err("an unparseable source is not copied");
    assert!(!to.exists());

    let names: Vec<_> = std::fs::read_dir(&*dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names.len(),
        3,
        "only the fixtures and broken.json: {names:?}"
    );
}

/// An existing destination spelled through a directory that is not there: reading that spelling
/// fails, so only a check on the destination's identity sees the file.
#[test]
fn an_existing_destination_under_another_spelling_refuses_and_is_untouched() {
    let dir = voices("respelled");
    let from = dir.join("kick-voice.json");

    for taken in ["shaped-vca.json", "kick-voice.json"] {
        let before = std::fs::read_to_string(dir.join(taken)).unwrap();
        let refusal = copy(&from, &dir.join("ghost/..").join(taken)).expect_err("no overwrite");
        assert!(refusal.message.contains("already exists"), "{refusal}");
        assert_eq!(std::fs::read_to_string(dir.join(taken)).unwrap(), before);
    }
    assert!(
        !dir.join("ghost").exists(),
        "a refused copy made a directory"
    );
}

/// `link/..` names the original's directory lexically and the link target's parent on disk. The
/// copy lands where its identity says, which is where it was validated.
#[cfg(unix)]
#[test]
fn a_destination_through_a_symlinked_directory_lands_where_it_was_validated() {
    let dir = voices("symlink");
    let from = dir.join("kick-voice.json");
    let inner = dir.parent().unwrap().join("other/inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::os::unix::fs::symlink(&inner, dir.join("link")).unwrap();

    let result = copy(&from, &dir.join("link/../kick-copy.json"))
        .expect("beside the original")
        .output;

    assert!(result.written, "{result:?}");
    assert!(dir.join("kick-copy.json").is_file());
    assert!(!inner.parent().unwrap().join("kick-copy.json").exists());
}

/// A library root holding a document of the destination's name must not stand in for the
/// destination: the copy would be checked against one directory and written into another.
#[test]
fn a_library_document_of_the_same_name_does_not_stand_in_for_the_destination() {
    let lib = voices("library");
    let project = lib.parent().unwrap().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let from = lib.join("kick-voice.json");
    std::fs::copy(&from, lib.join("kick-copy.json")).unwrap();
    let resolver = FsResolver::new(&project).with_root(lib.parent().unwrap());

    copy_through(&from, Path::new("voices/kick-copy.json"), &resolver)
        .expect_err("the project directory is not the original's");

    assert!(!project.join("voices").exists(), "written into the project");
}

/// The conservative side of the same lookup: a free name beside the original is refused while the
/// library root holds a document of that name.
#[test]
fn a_bare_name_the_library_root_already_holds_is_refused() {
    let dir = voices("shadowed");
    let from = dir.join("kick-voice.json");
    let lib = dir.parent().unwrap().join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    std::fs::copy(&from, lib.join("kick-copy.json")).unwrap();
    let resolver = FsResolver::for_document(&from.display().to_string()).with_root(&lib);

    let refusal = copy_through(&from, Path::new("kick-copy.json"), &resolver).expect_err("taken");

    assert!(refusal.message.contains("already exists"), "{refusal}");
    assert!(!dir.join("kick-copy.json").exists());
}

/// The copy's references resolve from the copy, whatever directory the resolver is based in.
#[test]
fn a_copy_is_validated_from_where_it_lands_not_from_the_resolvers_base() {
    let dir = voices("rebased");
    let elsewhere = dir.parent().unwrap().join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let (from, to) = (dir.join("kick-voice.json"), dir.join("kick-copy.json"));

    let result = copy_through(&from, &to, &FsResolver::new(&elsewhere))
        .expect("beside the original")
        .output;

    assert!(result.written && result.report.ok, "{result:?}");
    assert!(result.report.warnings.is_empty(), "{result:?}");
}

/// A reference to the destination is a missing file before the write and a cycle after it.
#[test]
fn a_document_that_references_its_own_destination_is_not_copied() {
    let dir = voices("selfref");
    let (from, to) = (dir.join("kick-voice.json"), dir.join("kick-copy.json"));
    let kick = std::fs::read_to_string(&from).unwrap();
    assert!(kick.contains("shaped-vca.json"));
    std::fs::write(&from, kick.replace("shaped-vca.json", "kick-copy.json")).unwrap();

    let refusal = copy(&from, &to).expect_err("it would nest itself");

    assert!(refusal.message.contains("references"), "{refusal}");
    assert!(!to.exists());
}
