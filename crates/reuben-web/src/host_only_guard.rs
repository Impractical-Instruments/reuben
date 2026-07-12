//! Safe-removal guard for the host-only [`crate::tool_schema`] module (WX-3, issue #417).
//!
//! The Phase-3 extraction (WX-14) deletes `tool_schema` — the web-chat agent's off-line
//! schema-artifact generator — from the public crate. That deletion must be provably safe: the
//! `wasm32-unknown-unknown` build (the shipped payload) must be UNAFFECTED by removing
//! `pub mod tool_schema;`. It is unaffected precisely when the module has zero runtime coupling —
//! nothing compiled into the wasm cdylib depends on it.
//!
//! These plain `cargo test` assertions document and enforce that property, so it holds by
//! construction rather than by reviewer vigilance:
//!
//!   1. [`tool_schema_is_gated_host_only`] — the declaration in `lib.rs` keeps its
//!      `#[cfg(not(target_arch = "wasm32"))]` gate, so the module is compiled OUT of the wasm
//!      build; removing the `pub mod` line changes nothing for that target.
//!   2. [`no_wasm_reachable_source_references_tool_schema`] — NONE of the source files that
//!      compile into the wasm cdylib (everything under `src/` except the host-only module itself
//!      and this guard) so much as names `tool_schema`. Since the module does not exist on wasm,
//!      such a reference is already a wasm compile error; asserting it here on the host makes the
//!      seam a greppable contract that also fails fast in the common `cargo test` run.
//!   3. [`tool_schema_is_reached_only_by_the_host_generator_and_its_staleness_test`] — the two
//!      sanctioned host reachers (the `gen_tool_schemas` example binary and the in-module
//!      staleness test) still pull the module in, documenting the whole of the issue's
//!      "referenced ONLY by `examples/gen_tool_schemas.rs` + its staleness test" clause.
//!
//! Together: the module is out of the wasm build (1), nothing wasm-reachable references it (2),
//! and its only reachers are host-side tooling (3) — so deleting it cannot break the wasm build.
//! CI's `cargo build/clippy --target wasm32-unknown-unknown` is the ultimate backstop for (2).

use std::fs;
use std::path::PathBuf;

/// The crate's `src/` directory, resolved at test time (tests run from `CARGO_MANIFEST_DIR`).
fn src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// The exact gated declaration `lib.rs` must carry. rustfmt (a CI gate) normalises attribute
/// spacing, so this canonical form is stable to match against.
const GATED_DECLARATION: &str = "#[cfg(not(target_arch = \"wasm32\"))]\npub mod tool_schema;";

#[test]
fn tool_schema_is_gated_host_only() {
    let lib = fs::read_to_string(src_dir().join("lib.rs")).expect("read src/lib.rs");
    assert!(
        lib.contains(GATED_DECLARATION),
        "WX-3 (#417): `tool_schema` must stay declared as\n\n    {}\n\nin src/lib.rs so it is \
         compiled OUT of the wasm build. Widening or dropping the `#[cfg(not(target_arch = \
         \"wasm32\"))]` gate re-admits it to the wasm payload (issue #227) and undoes the \
         safe-removal guarantee WX-14 relies on.",
        GATED_DECLARATION.replace('\n', "\n    "),
    );
}

#[test]
fn no_wasm_reachable_source_references_tool_schema() {
    // Every `src/*.rs` file compiles into the wasm cdylib EXCEPT:
    //   - `tool_schema.rs`      — the host-only module itself (gated out of wasm);
    //   - `lib.rs`              — carries the sanctioned gated `pub mod` declaration, checked by
    //                             `tool_schema_is_gated_host_only` above;
    //   - `host_only_guard.rs`  — this guard, which necessarily names the module in its asserts.
    // A `tool_schema` reference in any OTHER file would couple it into the shipped payload.
    const ALLOWED_TO_NAME_IT: &[&str] = &["tool_schema.rs", "lib.rs", "host_only_guard.rs"];

    let mut checked = 0usize;
    for entry in fs::read_dir(src_dir()).expect("read src/") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("utf-8 filename");
        if ALLOWED_TO_NAME_IT.contains(&name) {
            continue;
        }
        let body = fs::read_to_string(&path).expect("read source file");
        assert!(
            !body.contains("tool_schema"),
            "WX-3 (#417): src/{name} names `tool_schema`, but it compiles into the \
             `wasm32-unknown-unknown` cdylib. The host-only schema generator must have ZERO \
             runtime coupling so WX-14 can delete it without breaking the wasm build. Move the \
             dependency off the wasm-reachable surface.",
        );
        checked += 1;
    }
    // Guard against the guard silently checking nothing (e.g. a path regression): the crate always
    // has wasm-reachable modules besides the three exemptions (codec/decode/resolver/shell/bridge).
    assert!(
        checked >= 4,
        "expected several wasm-reachable sources to scan, found {checked}"
    );
}

#[test]
fn tool_schema_is_reached_only_by_the_host_generator_and_its_staleness_test() {
    // The sanctioned host reacher #1: the example binary pulls the module in by source path
    // (the crate is cdylib-only, so there is no rlib to `use reuben_web::tool_schema` from).
    let example = fs::read_to_string(src_dir().join("../examples/gen_tool_schemas.rs"))
        .expect("read examples/gen_tool_schemas.rs");
    assert!(
        example.contains("../src/tool_schema.rs") && example.contains("tool_schema::generate"),
        "WX-3 (#417): examples/gen_tool_schemas.rs must remain the host generator that pulls in \
         tool_schema (issue #354, ADR-0054 §3) — the artifact regen depends on it."
    );

    // The sanctioned host reacher #2: the staleness test lives inside the module itself.
    let module = fs::read_to_string(src_dir().join("tool_schema.rs")).expect("read tool_schema.rs");
    assert!(
        module.contains("fn committed_artifact_is_in_sync"),
        "WX-3 (#417): the `committed_artifact_is_in_sync` staleness test must stay in \
         src/tool_schema.rs — it is the in-module reacher the issue accounts for."
    );
}
