//! CI regression gate, micro layer (#30): deterministic instruction count of each operator's
//! `process` over the fixed 1 s schedule, via callgrind. Same rationale as `macro_iai` — counts are
//! CPU-independent and byte-stable, so the same-toolchain compare in CI flags a per-operator
//! regression without wall-clock flake (ADR-0019). The macro layer catches a graph getting slower;
//! this layer says which operator.
//!
//! `setup = OpHarness::for_kind` builds the harness (alloc, resource decode, event construction)
//! outside the measured region; only the render loop is counted.
//!
//! iai's `#[bench::…]` list is compile-time, so it can't iterate the registry — and a
//! `harness = false` bench can't host a libtest to introspect itself. The list below must therefore
//! mirror `bench_support::MICRO_IAI_KINDS` exactly; the #30 forcing function
//! (`bench_support::tests::iai_list_covers_every_workload`, in the `check` job) asserts that const
//! equals `WORKLOADS`, so adding an operator reds CI until both this list and `MICRO_IAI_KINDS` gain
//! the new kind.

use iai_callgrind::{library_benchmark, library_benchmark_group, main};
use reuben_core::bench_support::OpHarness;
use std::hint::black_box;

#[library_benchmark]
#[bench::add(args = ("add",), setup = OpHarness::for_kind)]
#[bench::chord(args = ("chord",), setup = OpHarness::for_kind)]
#[bench::clock(args = ("clock",), setup = OpHarness::for_kind)]
#[bench::delay(args = ("delay",), setup = OpHarness::for_kind)]
#[bench::differentiate(args = ("differentiate",), setup = OpHarness::for_kind)]
#[bench::djfilter(args = ("djfilter",), setup = OpHarness::for_kind)]
#[bench::envelope(args = ("envelope",), setup = OpHarness::for_kind)]
#[bench::euclid(args = ("euclid",), setup = OpHarness::for_kind)]
#[bench::filter(args = ("filter",), setup = OpHarness::for_kind)]
#[bench::harmony(args = ("harmony",), setup = OpHarness::for_kind)]
#[bench::integrate(args = ("integrate",), setup = OpHarness::for_kind)]
#[bench::lfo(args = ("lfo",), setup = OpHarness::for_kind)]
#[bench::m2s(args = ("m2s",), setup = OpHarness::for_kind)]
#[bench::map(args = ("map",), setup = OpHarness::for_kind)]
#[bench::mul(args = ("mul",), setup = OpHarness::for_kind)]
#[bench::noise(args = ("noise",), setup = OpHarness::for_kind)]
#[bench::osc_out(args = ("osc_out",), setup = OpHarness::for_kind)]
#[bench::oscillator(args = ("oscillator",), setup = OpHarness::for_kind)]
#[bench::output(args = ("output",), setup = OpHarness::for_kind)]
#[bench::pan(args = ("pan",), setup = OpHarness::for_kind)]
#[bench::power(args = ("power",), setup = OpHarness::for_kind)]
#[bench::reverb(args = ("reverb",), setup = OpHarness::for_kind)]
#[bench::sample(args = ("sample",), setup = OpHarness::for_kind)]
#[bench::sequencer(args = ("sequencer",), setup = OpHarness::for_kind)]
#[bench::snap(args = ("snap",), setup = OpHarness::for_kind)]
#[bench::strum(args = ("strum",), setup = OpHarness::for_kind)]
#[bench::transpose(args = ("transpose",), setup = OpHarness::for_kind)]
#[bench::voicer(args = ("voicer",), setup = OpHarness::for_kind)]
fn process(harness: OpHarness) -> f32 {
    black_box(harness.render())
}

library_benchmark_group!(name = micro; benchmarks = process);
main!(library_benchmark_groups = micro);
