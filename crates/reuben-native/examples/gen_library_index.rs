//! Regenerate the committed library index from the `instruments/` available-set.
//!
//! Run after adding or changing an instrument document:
//! `cargo run -p reuben-native --example gen_library_index`
//! The `library_index_is_in_sync` test fails if the committed file is stale.

use std::path::Path;

use reuben_core::registry::Registry;
use reuben_native::library::generate_library_index;

fn main() {
    let instruments = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../instruments");
    let index =
        generate_library_index(&instruments, &Registry::builtin()).expect("generate library index");
    let out = instruments.join("index.md");
    std::fs::write(&out, index).expect("write index");
    println!("wrote {}", out.display());
}
