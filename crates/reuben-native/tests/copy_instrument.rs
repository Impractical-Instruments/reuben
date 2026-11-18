//! Integration: `copy_instrument` over the real filesystem resolver, on a shipped voice that nests
//! a sibling — so "beside the original" and "another directory" are directories on a disk rather
//! than a test store's idea of one. Every case works on a private copy of the fixtures under the
//! system temp directory; nothing is written into `instruments/`.

use std::path::{Path, PathBuf};

use reuben_api::authoring::{self, CopyInstrument, ValidateInstrument};
use reuben_api::FsResolver;

/// A fresh `<temp>/<name>/voices/` holding `kick-voice.json` and the `shaped-vca.json` it nests.
fn voices(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("reuben_copy_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("voices");
    std::fs::create_dir_all(&dir).unwrap();
    let shipped = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instruments/voices");
    for file in ["kick-voice.json", "shaped-vca.json"] {
        std::fs::copy(shipped.join(file), dir.join(file)).unwrap();
    }
    dir
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir.parent().unwrap());
}

/// Copy as a host does: one resolver scoped to the document being copied.
fn copy(
    from: &Path,
    to: &Path,
) -> Result<authoring::Answer<authoring::EditResult>, authoring::Refusal> {
    let from = from.display().to_string();
    authoring::copy_instrument(
        &CopyInstrument {
            from: from.clone(),
            to: to.display().to_string(),
        },
        &FsResolver::for_document(&from),
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

    cleanup(&dir);
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

    cleanup(&dir);
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

    cleanup(&dir);
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

    cleanup(&dir);
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

    let names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(
        names.len(),
        3,
        "only the fixtures and broken.json: {names:?}"
    );

    cleanup(&dir);
}
