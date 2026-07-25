//! Integration: the shipped stereo examples and their device profile, driven through the very
//! window verb the `reuben validate` subcommand is, over the real filesystem resolver — so what
//! passes here is what the binary reports, not a parallel path that happens to agree.

use std::path::PathBuf;

use reuben_api::authoring::{self, ValidateInstrument};
use reuben_api::FsResolver;

/// Absolute path to this crate's frozen test fixtures (docs that are test coverage,
/// not library instruments).
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Validate a document at `source` exactly as the `validate` subcommand does: a document-scoped
/// resolver that **decodes** samples, and the window's verb.
///
/// Decoding is the part worth being careful about. Stat-ing instead would still pass every
/// assertion below — the fixtures reference no unreadable audio — while quietly testing a
/// resolver the binary does not use. `an_undecodable_sample_is_a_warning` is what makes the
/// setting load-bearing here.
fn validate_source(source: &str) -> authoring::Report {
    authoring::validate_instrument(
        &ValidateInstrument {
            source: source.to_string(),
        },
        &FsResolver::for_document(source),
    )
    .unwrap_or_else(|refusal| panic!("{source} should be readable: {refusal}"))
    .output
}

/// Validate one frozen fixture by name.
fn validate(name: &str) -> authoring::Report {
    validate_source(&fixtures_dir().join(name).display().to_string())
}

#[test]
fn validate_accepts_the_stereo_autopan_example() {
    let report = validate("stereo-autopan.json");
    assert!(
        report.ok && report.errors.is_empty(),
        "stereo-autopan.json should validate: {:?}",
        report.errors
    );
}

#[test]
fn validate_accepts_the_stereo_sub_example() {
    // The multichannel-out demo: three channel-bound output pipes (mains + sub send).
    let report = validate("stereo-sub.json");
    assert!(
        report.ok && report.errors.is_empty(),
        "stereo-sub.json should validate: {:?}",
        report.errors
    );
    assert!(
        report.warnings.is_empty(),
        "stereo-sub.json should validate warning-clean: {:?}",
        report.warnings
    );
}

/// A sample that is present but is not decodable audio is a warning, because `validate` is the dry
/// run of the load `play` will do for real — and a resolver that only stats the file reports a
/// document legal that cannot be heard.
///
/// This pins the one setting on which `validate`'s resolver differs from the introspection reads'.
/// The distinction is invisible in every other assertion in this file, which is exactly how it went
/// missing once already.
#[test]
fn an_undecodable_sample_is_a_warning() {
    let dir = std::env::temp_dir().join("reuben_cli_undecodable_sample");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    std::fs::write(dir.join("hit.wav"), b"not a wav at all").expect("write the decoy sample");
    let source = dir.join("inst.json").display().to_string();
    std::fs::write(
        &source,
        r#"{
            "format_version": 3,
            "instrument": "decode-check",
            "resources": { "hit": "hit.wav" },
            "nodes": [ { "type": "sample", "address": "/s", "sample": "hit" } ]
        }"#,
    )
    .expect("write the document");

    let report = validate_source(&source);
    // Advisory, not fatal: an unplayable sample does not make the graph illegal, and `validate`
    // keeps exit 0 on warnings. What must not happen is silence.
    assert!(
        report.ok,
        "a decode failure is advisory: {:?}",
        report.errors
    );
    assert!(
        report.warnings.iter().any(|w| w.message.contains("hit")),
        "the undecodable sample must be reported: {:?}",
        report.warnings
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shipped_stereo_sub_io_map_parses() {
    // The example device profile frozen next to the demo stays structurally
    // valid: mains identity-mapped, the sub send routed to device channel 3.
    let profile =
        reuben_native::profile::DeviceProfile::load(&fixtures_dir().join("stereo-sub.io-map.json"))
            .expect("stereo-sub.io-map.json should parse as a device profile");
    let map = &profile.output.map;
    assert_eq!(map.get(&0), Some(&0));
    assert_eq!(map.get(&1), Some(&1));
    assert_eq!(map.get(&2), Some(&3));
}
