//! Local wall-clock per-operator micro benchmark (#30): each operator's `process` driven directly
//! for the fixed 1 s schedule, reported as samples/sec (÷ 48 000 == ×realtime). The diagnostic
//! companion to `macro_criterion` — when the macro layer flags a graph got slower, this attributes
//! it to an operator. Dev-facing; never runs in CI (the gate is `micro_iai`, ADR-0019).
//!
//! Iterates [`WORKLOADS`] at runtime, so a new operator is benched automatically once it has a
//! workload entry. Run via `cargo reuben-core-bench --bench micro_criterion` (the alias enables the
//! `bench` feature this target requires).

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use reuben_core::bench_support::{OpHarness, BLOCKS, BLOCK_SIZE, WORKLOADS};

fn operators(c: &mut Criterion) {
    let mut group = c.benchmark_group("operator");
    // One iteration renders BLOCKS*BLOCK_SIZE frames; throughput => samples/sec.
    group.throughput(Throughput::Elements((BLOCKS * BLOCK_SIZE) as u64));
    for w in WORKLOADS {
        group.bench_function(w.kind, |b| {
            // Rebuild fresh state per iteration so operator state never carries over; only the
            // render loop is timed.
            b.iter_batched(
                || OpHarness::for_kind(w.kind),
                |h| h.render(),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, operators);
criterion_main!(benches);
