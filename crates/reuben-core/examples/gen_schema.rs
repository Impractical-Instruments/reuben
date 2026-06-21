//! Regenerate the committed instrument JSON Schema from the operator descriptors.
//!
//! Run after adding/changing an operator or its params:
//! `cargo run -p reuben-core --example gen_schema`
//! The `schema_is_in_sync` test fails if the committed file is stale.

use std::path::Path;

use reuben_core::registry::Registry;
use reuben_core::schema;

fn main() {
    let out = Path::new(env!("CARGO_MANIFEST_DIR")).join("schema/instrument.schema.json");
    std::fs::create_dir_all(out.parent().unwrap()).expect("create schema dir");
    std::fs::write(&out, schema::generate_pretty(&Registry::builtin())).expect("write schema");
    println!("wrote {}", out.display());
}
