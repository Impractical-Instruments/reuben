//! Regenerate the micro-bench census from the operator registry: the kinds file the perf gate reads
//! and the `micro_bench_ops!` block in the iai bench.
//!
//! Run after adding, renaming or removing an operator:
//! `cargo run -p reuben-core --example gen_bench_census`
//! `reuben-document/tests/bench_census.rs` fails while either file is out of date.

use std::path::Path;

use reuben_core::bench_census::{
    kinds, render_iai_census, render_kinds_file, splice_iai_census, IAI_BENCH, KINDS_FILE,
};
use reuben_core::Registry;

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root");
    let kinds = kinds(&Registry::builtin());

    let kinds_path = root.join(KINDS_FILE);
    std::fs::write(&kinds_path, render_kinds_file(&kinds)).expect("write kinds file");
    println!("wrote {}", kinds_path.display());

    let iai_path = root.join(IAI_BENCH);
    let src = std::fs::read_to_string(&iai_path).expect("read iai bench");
    let spliced = splice_iai_census(&src, &render_iai_census(&kinds))
        .unwrap_or_else(|| panic!("{IAI_BENCH} has no `micro_bench_ops! {{ … }}` block"));
    std::fs::write(&iai_path, spliced).expect("write iai bench");
    println!("wrote {}", iai_path.display());
}
