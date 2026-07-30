//! CI regression gate, construct layer (see rules: web-product-process): deterministic
//! instruction count of parse + build + instantiate at three node counts per shape.
//!
//! The macro and micro layers gate *render* — cost paid per block, forever. This one gates the
//! cost paid **once per Swap**, on the caller's thread. In the browser that thread is the main
//! thread, so a superlinear build is a frozen tab rather than a slow background job, and the two
//! consumers that make large graphs ordinary — resident song sections and agent-generated
//! instruments — put no human-scale ceiling on node count.
//!
//! Each shape is benched at N, 2N and 4N nodes. Two things are gated off that:
//!
//! - **Absolute Ir per case**, against the baseline ref, like every other layer.
//! - **The growth factor between consecutive sizes**, by `perf-gate.sh`. This is the check the
//!   per-case comparison cannot make: a per-node scan is a perfectly ordinary-looking number at
//!   any single size, and only a size *pair* distinguishes a build that doubles with its input
//!   from one that quadruples.
//!
//! `setup` generates the document outside the measured region, so JSON *generation* is never
//! counted — only the engine work of turning it into a Plan.
//!
//! Case ids carry the **node count**, not the shape parameter, because that is the number the
//! growth-factor gate divides: `perf-gate.sh` reads `<shape>_n<nodes>` back out of the harvested
//! Ir records to pair consecutive sizes. Renaming an id unpairs it — and, as in the micro layer,
//! unmatches its baseline and its history. The `nest` shape builds out to `4 * cells + 1` nodes,
//! so its ids name the nominal size rather than the exact one; the gate divides Ir, and the odd
//! node cancels.

mod common;

use common::construct::{construct, deep_doc, nest_doc, wide_doc};
use iai_callgrind::{library_benchmark, library_benchmark_group, main};
use std::hint::black_box;

/// Generate a wide document of `2 * leaves` nodes, outside the measured region.
fn wide(leaves: usize) -> String {
    wide_doc(leaves)
}

/// Generate a deep document of `stages + 2` nodes, outside the measured region.
fn deep(stages: usize) -> String {
    deep_doc(stages)
}

/// Generate a nested document of `4 * cells + 1` nodes, outside the measured region.
fn nest(cells: usize) -> String {
    nest_doc(cells)
}

#[library_benchmark]
#[bench::wide_n2048(args = (1024,), setup = wide)]
#[bench::wide_n4096(args = (2048,), setup = wide)]
#[bench::wide_n8192(args = (4096,), setup = wide)]
#[bench::deep_n2048(args = (2046,), setup = deep)]
#[bench::deep_n4096(args = (4094,), setup = deep)]
#[bench::deep_n8192(args = (8190,), setup = deep)]
#[bench::nest_n2048(args = (512,), setup = nest)]
#[bench::nest_n4096(args = (1024,), setup = nest)]
#[bench::nest_n8192(args = (2048,), setup = nest)]
fn build(doc: String) -> usize {
    black_box(construct(&doc))
}

library_benchmark_group!(name = construct_group; benchmarks = build);
main!(library_benchmark_groups = construct_group);
