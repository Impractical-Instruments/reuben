//! Local wall-clock view of the **construct** workload: parse + build + instantiate, at a sweep
//! of node counts in two shapes. The sibling of [`construct_iai`](../construct_iai.rs), which is
//! what actually gates CI.
//!
//! Throughput is reported in `elem/s` where an element is one document node, so the number to
//! read is *per-node cost*: a linear build holds `elem/s` flat across the sweep, and a per-node
//! scan halves it at every doubling. That is the whole point of sweeping rather than benching one
//! size — an absolute figure at a single node count says nothing about which of the two it is.

mod common;

use common::construct::{construct, deep_doc, nest_doc, wide_doc};
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use std::hint::black_box;

/// Leaf counts for the wide shape; the document has `2 * leaves` nodes.
const WIDE_LEAVES: &[usize] = &[64, 128, 256, 512, 1024, 2048, 4096];
/// Chain lengths for the deep shape; the document has `stages + 2` nodes.
const DEEP_STAGES: &[usize] = &[126, 254, 510, 1022, 2046, 4094, 8190];
/// Cell counts for the nested shape; the document builds out to `4 * cells + 1` nodes.
const NEST_CELLS: &[usize] = &[32, 64, 128, 256, 512, 1024, 2048];

fn construct_bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("construct");
    for &leaves in WIDE_LEAVES {
        let json = wide_doc(leaves);
        let nodes = 2 * leaves;
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(BenchmarkId::new("wide", nodes), &json, |b, json| {
            b.iter(|| black_box(construct(json, nodes)))
        });
    }
    for &stages in DEEP_STAGES {
        let json = deep_doc(stages);
        let nodes = stages + 2;
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(BenchmarkId::new("deep", nodes), &json, |b, json| {
            b.iter(|| black_box(construct(json, nodes)))
        });
    }
    for &cells in NEST_CELLS {
        let json = nest_doc(cells);
        let nodes = 4 * cells + 1;
        group.throughput(Throughput::Elements(nodes as u64));
        group.bench_with_input(BenchmarkId::new("nest", nodes), &json, |b, json| {
            b.iter(|| black_box(construct(json, nodes)))
        });
    }
    group.finish();
}

criterion_group!(benches, construct_bench);
criterion_main!(benches);
