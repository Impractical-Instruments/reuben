//! Local wall-clock macro benchmark: end-to-end `render_block` throughput per
//! instrument, reported as samples/sec (÷ 48 000 == ×realtime). Dev-facing — run
//! `cargo bench -p reuben-core --bench macro_criterion`; compare across changes
//! with `--save-baseline <name>` / `--baseline <name>`. Never runs in CI (shared
//! runners are too noisy for wall-clock); the CI gate is `macro_iai` (ADR-0019).

mod common;

use common::{build_state, FIXTURE_NAMES, TOTAL_SAMPLES};
use criterion::{criterion_group, criterion_main, BatchSize, Criterion, Throughput};

fn render_block(c: &mut Criterion) {
    let mut group = c.benchmark_group("render_block");
    // One iteration renders TOTAL_SAMPLES frames; throughput => samples/sec.
    group.throughput(Throughput::Elements(TOTAL_SAMPLES));
    for &name in FIXTURE_NAMES {
        group.bench_function(name, |b| {
            // Rebuild fresh state per iteration so voice/envelope state never
            // carries over; only the render loop is timed.
            b.iter_batched(
                || build_state(name),
                |state| state.render(),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, render_block);
criterion_main!(benches);
