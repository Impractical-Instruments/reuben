//! Guards the checked-in M1 verification fixtures against rot.
//!
//! The device-gap ritual (`docs/mcp-swap-ritual.md`) and the demo bar
//! (`docs/rituals/m1-demo-bar.md`) are the scripted-human-ritual half of verification; their
//! fixed, checked-in documents live in `tests/fixtures/m1/`. This test keeps them honest — every
//! fixture must still **load and plan** through the single loader authority, and the demo prompt
//! text must stay the exact fixed string the ritual hands the agent — so a later change that
//! breaks a fixture reds CI here instead of surfacing only when someone next runs the ritual on
//! hardware. The *automated* half of verification is `structure_server.rs`; the two are separate
//! binaries on purpose (one wire test, one fixture guard).

use std::path::{Path, PathBuf};

use reuben_api::authoring::{validate_instrument, ValidateInstrument};
use reuben_api::FsResolver;

/// Absolute path into this crate's `tests/fixtures/m1/` tree.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/m1")
        .join(name)
}

/// Load + plan a fixture instrument through the window's own validate verb — the same one the
/// structure channel's swap runs and the CLI's `validate` serves — asserting it is valid with no
/// errors.
fn assert_valid(name: &str) {
    let source = fixture(name).display().to_string();
    let report = validate_instrument(
        &ValidateInstrument {
            source: source.clone(),
        },
        &FsResolver::for_document(&source),
    )
    .unwrap_or_else(|refusal| panic!("M1 fixture {source} could not be validated: {refusal}"))
    .output;
    assert!(
        report.ok,
        "M1 fixture {name} must load + plan cleanly, got errors: {:?}",
        report.errors
    );
}

#[test]
fn demo_bass_starting_instrument_is_valid() {
    // The demo bar's fixed starting instrument — must be a real playable bass so
    // `reuben play` sounds and the agent's edit has something to change.
    assert_valid("bass.json");
}

#[test]
fn device_gap_swap_target_is_valid() {
    // The M1 restart-swap device-gap ritual's fixed second document — must load so the swap
    // installs and the audible gap/resume is exercised on hardware.
    assert_valid("device-gap-swap.json");
}

#[test]
fn demo_prompt_is_the_fixed_text() {
    // The demo prompt is part of the fixture: pinned so the acceptance ritual is
    // reproducible run to run and a stray edit can't silently reword it.
    let prompt = std::fs::read_to_string(fixture("prompt.txt")).expect("read demo prompt");
    assert_eq!(prompt.trim(), "make the bass rounder and add a dub delay");
}
