//! Regenerate the committed web-chat tool-schema artifact from reuben-core (issue #354, ADR-0054 §3).
//!
//! Run after adding/changing an operator, the instrument format, or a tool's input shape:
//! `cargo run --example gen_tool_schemas` (from `crates/reuben-web`)
//! The `committed_artifact_is_in_sync` test (src/tool_schema.rs) fails if the committed file is stale.
//!
//! reuben-web is a cdylib-only crate (no rlib for an example to link, see Cargo.toml), so the
//! generator module is pulled in by source path rather than as `use reuben_web::tool_schema`. It
//! compiles against the same reuben-core it derives from, so the artifact cannot drift from core.

#[path = "../src/tool_schema.rs"]
mod tool_schema;

use std::path::Path;

fn main() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("js/tool-schemas.generated.json");
    std::fs::create_dir_all(out.parent().unwrap()).expect("create js dir");
    std::fs::write(&out, tool_schema::generate_pretty()).expect("write tool-schemas artifact");
    println!("wrote {}", out.display());
}
