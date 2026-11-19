//! Staleness gate for the generated micro-bench census: the committed kinds file must equal a fresh
//! generation from the operator registry, and the iai bench's `micro_bench_ops!` block must equal
//! one generated from that kinds file.

use reuben_core::bench_census::{
    kinds, parse_kinds_file, render_iai_census, render_kinds_file, splice_iai_census, IAI_BENCH,
    KINDS_FILE, REGENERATE,
};
use reuben_core::Registry;

/// Compile-time bound, so a deleted file fails as loudly as a stale one.
const COMMITTED_KINDS: &str = include_str!("../benches/micro_kinds.txt");
const COMMITTED_IAI: &str = include_str!("../benches/micro_iai.rs");

#[test]
fn kinds_file_is_in_sync_with_the_registry() {
    let fresh = render_kinds_file(&kinds(&Registry::builtin()));
    assert_eq!(
        fresh, COMMITTED_KINDS,
        "{KINDS_FILE} is stale against the operator registry — run `{REGENERATE}`"
    );
}

#[test]
fn iai_census_is_in_sync_with_the_kinds_file() {
    let census = render_iai_census(&parse_kinds_file(COMMITTED_KINDS));
    let fresh = splice_iai_census(COMMITTED_IAI, &census)
        .unwrap_or_else(|| panic!("{IAI_BENCH} has no `micro_bench_ops! {{ … }}` block"));
    // Not `assert_eq!`: the diff would be the whole bench file.
    assert!(
        fresh == COMMITTED_IAI,
        "the `micro_bench_ops!` census in {IAI_BENCH} is stale against {KINDS_FILE} — run \
         `{REGENERATE}`"
    );
}

/// criterion iterates `all_workloads` and iai benches the generated census, so the two must be the
/// same set of kinds, or an operator could be benched locally yet escape the gate.
#[cfg(feature = "bench")]
#[test]
fn every_workload_is_in_the_committed_kinds_file() {
    let workloads: Vec<&str> = reuben_core::bench_support::all_workloads()
        .iter()
        .map(|w| w.kind)
        .collect();
    assert_eq!(
        workloads,
        parse_kinds_file(COMMITTED_KINDS),
        "{KINDS_FILE} is stale — run `{REGENERATE}`"
    );
}
