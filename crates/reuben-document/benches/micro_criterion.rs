//! Local wall-clock per-operator micro benchmark: each operator's `process` driven directly
//! for the fixed 1 s schedule, reported as samples/sec (÷ 48 000 == ×realtime). The diagnostic
//! companion to `macro_criterion` — when the macro layer flags a graph got slower, this attributes
//! it to an operator. Dev-facing; never runs in CI (the gate is `micro_iai`).
//!
//! Iterates [`all_workloads`] at runtime, so a new operator is benched as soon as it is
//! registered. Run via `cargo reuben-bench --bench micro_criterion` (the alias enables the
//! `bench` feature this target requires).

use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};
use reuben_core::bench_support::{all_workloads, OpHarness, BLOCKS, BLOCK_SIZE};

fn operators(c: &mut Criterion) {
    let mut group = c.benchmark_group("operator");
    // One iteration renders BLOCKS*BLOCK_SIZE frames; throughput => samples/sec.
    group.throughput(Throughput::Elements((BLOCKS * BLOCK_SIZE) as u64));
    for w in all_workloads() {
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
