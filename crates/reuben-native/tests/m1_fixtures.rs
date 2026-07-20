//! Guards the checked-in M1 verification fixtures against rot.
//!
//! The device-gap ritual (`docs/mcp-swap-ritual.md`) and the #220 demo bar
//! (`docs/rituals/m1-demo-bar.md`) are scripted human tests: a human runs fixed commands against
//! fixed, checked-in documents. Those documents live in `tests/fixtures/m1/`, co-located with the
//! golden live-server test. This test keeps them honest — every fixture must still **load and
//! plan** through the single loader authority, and the demo prompt text must stay
//! the exact fixed string the ritual hands the agent — so a later change that breaks a fixture
//! reds CI here instead of surfacing only when someone next runs the manual ritual on hardware.
//!
//! These are the *human*-ritual fixtures; the *automated* half of the M1 harness is
//! `structure_server.rs`. The two are separate binaries on purpose (one wire test, one fixture
//! guard), so neither reads as the other's concern.

use std::path::{Path, PathBuf};

use reuben_core::introspect::validate;
use reuben_core::Registry;
use reuben_native::resources::FsResolver;

/// Absolute path into this crate's `tests/fixtures/m1/` tree.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/m1")
        .join(name)
}

/// Load + plan a fixture instrument through the real loader authority (the same `validate` the
/// structure channel's swap verb runs), asserting it is valid with no errors.
fn assert_valid(name: &str) {
    let path = fixture(name);
    let json = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read fixture {}: {e}", path.display()));
    let resolver = FsResolver::for_instrument(&path);
    let report = validate(&json, &Registry::builtin(), &resolver);
    assert!(
        report.ok,
        "M1 fixture {name} must load + plan cleanly, got errors: {:?}",
        report.errors
    );
}

#[test]
fn demo_bass_starting_instrument_is_valid() {
    // (c) The #220 demo bar's fixed starting instrument — must be a real playable bass so
    // `reuben play` sounds and the agent's edit has something to change.
    assert_valid("bass.json");
}

#[test]
fn device_gap_swap_target_is_valid() {
    // (b) The M1 restart-swap device-gap ritual's fixed second document — must load so the swap
    // installs and the audible gap/resume is exercised on hardware.
    assert_valid("device-gap-swap.json");
}

#[test]
fn demo_prompt_is_the_fixed_text() {
    // (c) The demo prompt is part of the fixture: pinned so the epic-level acceptance ritual is
    // reproducible run to run and a stray edit can't silently reword it.
    let prompt = std::fs::read_to_string(fixture("prompt.txt")).expect("read demo prompt");
    assert_eq!(prompt.trim(), "make the bass rounder and add a dub delay");
}
