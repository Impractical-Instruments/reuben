//! CI regression gate: deterministic instruction count of end-to-end `render_block`
//! per instrument, via callgrind. Instruction counts are CPU-speed-independent and
//! byte-stable across runs, so the same-toolchain compare in CI flags real
//! regressions without wall-clock flake.
//!
//! `setup = build_state` runs the load/instantiate outside the measured region;
//! only the render loop is counted.

mod common;

use common::{build_state, BenchState};
use iai_callgrind::{library_benchmark, library_benchmark_group, main};
use std::hint::black_box;

#[library_benchmark]
#[bench::reverb(args = ("reverb",), setup = build_state)]
#[bench::echo(args = ("echo",), setup = build_state)]
#[bench::auto_filter(args = ("auto-filter",), setup = build_state)]
#[bench::sampler_arp(args = ("sampler-arp",), setup = build_state)]
#[bench::autotune(args = ("autotune",), setup = build_state)]
fn render(state: BenchState) -> f32 {
    black_box(state.render())
}

library_benchmark_group!(name = macro_render; benchmarks = render);
main!(library_benchmark_groups = macro_render);
