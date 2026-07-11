//! Integration: the shipped stereo examples and their device profile, driven through the
//! `reuben_native::cli` re-export of core's introspection (ADR-0020, ADR-0044 §3) with the
//! real filesystem resolver — proving the re-export preserves the CLI surface. The pure
//! introspection tests moved with the code to `reuben_core::introspect` (issue #309).

use std::path::PathBuf;

use reuben_core::Registry;
use reuben_native::cli::validate;
use reuben_native::resources::FsResolver;

/// Absolute path to this crate's frozen test fixtures (docs that are test coverage,
/// not library instruments).
fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

#[test]
fn validate_accepts_the_stereo_autopan_example() {
    let dir = fixtures_dir();
    let json =
        std::fs::read_to_string(dir.join("stereo-autopan.json")).expect("read stereo-autopan.json");
    let report = validate(&json, &Registry::builtin(), &FsResolver::new(&dir));
    assert!(
        report.ok && report.errors.is_empty(),
        "stereo-autopan.json should validate: {:?}",
        report.errors
    );
}

#[test]
fn validate_accepts_the_stereo_sub_example() {
    // The multichannel-out demo (ADR-0038): three channel-bound output pipes (mains + sub send).
    let dir = fixtures_dir();
    let json = std::fs::read_to_string(dir.join("stereo-sub.json")).expect("read stereo-sub.json");
    let report = validate(&json, &Registry::builtin(), &FsResolver::new(&dir));
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
    // The example device profile frozen next to the demo (ADR-0038 §6) stays structurally
    // valid: mains identity-mapped, the sub send routed to device channel 3.
    let profile =
        reuben_native::profile::DeviceProfile::load(&fixtures_dir().join("stereo-sub.io-map.json"))
            .expect("stereo-sub.io-map.json should parse as a device profile");
    let map = &profile.output.map;
    assert_eq!(map.get(&0), Some(&0));
    assert_eq!(map.get(&1), Some(&1));
    assert_eq!(map.get(&2), Some(&3));
}
