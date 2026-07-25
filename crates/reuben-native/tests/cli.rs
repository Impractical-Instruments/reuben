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

/// Validate one fixture by name, exactly as the CLI does: a document-scoped resolver over the
/// fixture directory, and the window's verb.
fn validate(name: &str) -> authoring::Report {
    let source = fixtures_dir().join(name).display().to_string();
    authoring::validate_instrument(
        &ValidateInstrument {
            source: source.clone(),
        },
        &FsResolver::for_document(&source),
    )
    .unwrap_or_else(|refusal| panic!("{name} should be readable: {refusal}"))
    .output
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
