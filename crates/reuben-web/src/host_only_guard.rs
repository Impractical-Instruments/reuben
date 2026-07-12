//! Safe-removal guard for the host-only [`crate::tool_schema`] module (WX-3, issue #417).
//!
//! The Phase-3 extraction (WX-14) deletes `tool_schema` — the web-chat agent's off-line
//! schema-artifact generator — from the public crate. That deletion must be provably safe: the
//! `wasm32-unknown-unknown` build (the shipped payload) must be UNAFFECTED by removing
//! `pub mod tool_schema;`. It is unaffected precisely when the module has zero runtime coupling —
//! nothing the wasm C-ABI surface reaches depends on it.
//!
//! These plain `cargo test` assertions document and enforce that property from two directions, so
//! it holds by construction rather than by reviewer vigilance:
//!
//!   1. [`tool_schema_is_gated_host_only`] — the module declaration in `lib.rs` carries the
//!      `#[cfg(not(target_arch = "wasm32"))]` gate, so it is compiled OUT of the wasm build. If the
//!      gate is ever widened to include wasm (payload bloat + a coupling foothold), this trips.
//!   2. [`wasm_c_abi_surface_never_references_tool_schema`] — neither the `#[no_mangle]` C-ABI
//!      shims (`bridge.rs`) nor the target-agnostic lifecycle (`shell.rs`) so much as name
//!      `tool_schema`. Since the module does not exist on wasm, such a reference would already be a
//!      wasm compile error; asserting it here on the host makes the seam a first-class, greppable
//!      contract that fails fast in the common `cargo test` run too.
//!
//! Together: the module is out of the wasm build (1) AND nothing wasm-reachable references it (2),
//! so deleting it cannot break the wasm build — exactly WX-3's "done when". CI's
//! `cargo build --target wasm32-unknown-unknown` + `cargo clippy --target wasm32-unknown-unknown`
//! remain the ultimate backstop for direction (2).

/// The wasm-reachable source files: the raw C-ABI shims and the target-agnostic lifecycle. Both
/// are compiled into the `wasm32-unknown-unknown` cdylib, so a `tool_schema` reference from either
/// would couple the module into the shipped payload. (`include_str!` reads the file regardless of
/// `bridge`'s own `#[cfg(target_arch = "wasm32")]` gate, so this runs on the host.)
const WASM_REACHABLE_SOURCES: &[(&str, &str)] = &[
    ("bridge.rs", include_str!("bridge.rs")),
    ("shell.rs", include_str!("shell.rs")),
];

/// The exact gated declaration `lib.rs` must carry. rustfmt (a CI gate) normalises attribute
/// spacing, so this canonical form is stable to match against.
const GATED_DECLARATION: &str = "#[cfg(not(target_arch = \"wasm32\"))]\npub mod tool_schema;";

#[test]
fn tool_schema_is_gated_host_only() {
    let lib = include_str!("lib.rs");
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
fn wasm_c_abi_surface_never_references_tool_schema() {
    for (name, src) in WASM_REACHABLE_SOURCES {
        assert!(
            !src.contains("tool_schema"),
            "WX-3 (#417): {name} references `tool_schema`, but it is a wasm-reachable source. \
             The host-only schema generator must have ZERO runtime coupling so WX-14 can delete \
             it without breaking the `wasm32-unknown-unknown` build. Move the dependency off the \
             wasm C-ABI surface.",
        );
    }
}
