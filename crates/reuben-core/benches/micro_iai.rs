//! CI regression gate, micro layer (#30): deterministic instruction count of each operator's
//! `process` over the fixed 1 s schedule, via callgrind. Same rationale as `macro_iai` — counts are
//! CPU-independent and byte-stable, so the same-toolchain compare in CI flags a per-operator
//! regression without wall-clock flake (ADR-0019). The macro layer catches a graph getting slower;
//! this layer says which operator.
//!
//! The [`setup`] wrapper builds the harness (alloc, resource decode, event construction) outside the
//! measured region; only the render loop is counted. It returns `None` for a kind the CI perf gate
//! asked to skip (a PR-new operator with no baseline — see [`setup`]), so that operator is dropped
//! from the comparison instead of crashing the baseline run.
//!
//! iai's `#[bench::…]` list is compile-time, so it can't iterate the registry — and a
//! `harness = false` bench can't host a libtest to introspect itself. The list below must therefore
//! mirror `bench_support::MICRO_IAI_KINDS` exactly; the #30 forcing function
//! (`bench_support::tests::iai_list_covers_every_workload`, in the `check` job) asserts that const
//! equals `WORKLOADS`, so adding an operator reds CI until both this list and `MICRO_IAI_KINDS` gain
//! the new kind.

use iai_callgrind::{library_benchmark, library_benchmark_group, main, LibraryBenchmarkConfig};
use reuben_core::bench_support::OpHarness;
use std::hint::black_box;

/// Build the harness for `kind` — unless the CI perf gate listed it in `REUBEN_MICRO_BENCH_SKIP`, in
/// which case return `None` and skip it.
///
/// The gate reuses THIS (HEAD) bench against the baseline commit's swapped-in `src/`. An operator the
/// PR added — or renamed — isn't in the baseline registry, so `OpHarness::for_kind` would panic there
/// and abort the whole micro layer (the masking bug on #104: one renamed operator skipped EVERY
/// operator's gate). The gate lists those baseline-absent kinds and passes the *same* value to both
/// the baseline and PR runs, so a new operator is skipped symmetrically — it has no baseline to
/// compare against — while every operator that existed at the base is still benched.
///
/// This skip MUST live in the bench harness, not in `bench_support`: the gate swaps `bench_support`
/// (in reuben-core/src) to the baseline ref, so any skip logic there would be swapped *out* exactly
/// when the baseline run needs it. The harness is never swapped, and `for_kind` is the only public
/// surface it touches — so for a skipped kind `for_kind` is simply never called, on either side.
fn setup(kind: &str) -> Option<OpHarness> {
    let skip = std::env::var("REUBEN_MICRO_BENCH_SKIP")
        .is_ok_and(|v| v.split(',').any(|k| k.trim() == kind));
    (!skip).then(|| OpHarness::for_kind(kind))
}

#[library_benchmark]
#[bench::abs(args = ("abs_f32_signal",), setup = setup)]
#[bench::abs_value(args = ("abs_f32_value",), setup = setup)]
#[bench::add(args = ("add_f32_signal",), setup = setup)]
#[bench::add_value(args = ("add_f32_value",), setup = setup)]
#[bench::chord(args = ("chord",), setup = setup)]
#[bench::clamp(args = ("clamp_f32_signal",), setup = setup)]
#[bench::clamp_value(args = ("clamp_f32_value",), setup = setup)]
#[bench::clock(args = ("clock",), setup = setup)]
#[bench::compressor(args = ("compressor",), setup = setup)]
#[bench::delay(args = ("delay",), setup = setup)]
#[bench::differentiate(args = ("differentiate_f32_signal",), setup = setup)]
#[bench::div(args = ("div_f32_signal",), setup = setup)]
#[bench::div_value(args = ("div_f32_value",), setup = setup)]
#[bench::djfilter(args = ("djfilter",), setup = setup)]
#[bench::envelope(args = ("envelope",), setup = setup)]
#[bench::euclid(args = ("euclid",), setup = setup)]
#[bench::filter(args = ("filter",), setup = setup)]
#[bench::granulator(args = ("granulator",), setup = setup)]
#[bench::harmony(args = ("harmony",), setup = setup)]
#[bench::integrate(args = ("integrate_f32_signal",), setup = setup)]
#[bench::lfo(args = ("lfo",), setup = setup)]
#[bench::m2s(args = ("m2s",), setup = setup)]
#[bench::map(args = ("map_f32_signal",), setup = setup)]
#[bench::map_value(args = ("map_f32_value",), setup = setup)]
#[bench::max(args = ("max_f32_signal",), setup = setup)]
#[bench::max_value(args = ("max_f32_value",), setup = setup)]
#[bench::min(args = ("min_f32_signal",), setup = setup)]
#[bench::min_value(args = ("min_f32_value",), setup = setup)]
#[bench::modulo(args = ("modulo_f32_signal",), setup = setup)]
#[bench::modulo_value(args = ("modulo_f32_value",), setup = setup)]
#[bench::mul(args = ("mul_f32_signal",), setup = setup)]
#[bench::mul_value(args = ("mul_f32_value",), setup = setup)]
#[bench::negate(args = ("negate_f32_signal",), setup = setup)]
#[bench::negate_value(args = ("negate_f32_value",), setup = setup)]
#[bench::noise(args = ("noise",), setup = setup)]
#[bench::osc_out(args = ("osc_out",), setup = setup)]
#[bench::oscillator(args = ("oscillator",), setup = setup)]
#[bench::output(args = ("output",), setup = setup)]
#[bench::overhead(args = ("overhead",), setup = setup)]
#[bench::pan(args = ("pan",), setup = setup)]
#[bench::power(args = ("power_f32_signal",), setup = setup)]
#[bench::power_value(args = ("power_f32_value",), setup = setup)]
#[bench::reciprocal(args = ("reciprocal_f32_signal",), setup = setup)]
#[bench::reciprocal_value(args = ("reciprocal_f32_value",), setup = setup)]
#[bench::resonator(args = ("resonator",), setup = setup)]
#[bench::reverb(args = ("reverb",), setup = setup)]
#[bench::sample(args = ("sample",), setup = setup)]
#[bench::saturator(args = ("saturator",), setup = setup)]
#[bench::sequencer(args = ("sequencer",), setup = setup)]
#[bench::snap(args = ("snap",), setup = setup)]
#[bench::strum(args = ("strum",), setup = setup)]
#[bench::sub(args = ("sub_f32_signal",), setup = setup)]
#[bench::sub_value(args = ("sub_f32_value",), setup = setup)]
#[bench::subpatch(args = ("subpatch",), setup = setup)]
#[bench::transpose(args = ("transpose",), setup = setup)]
#[bench::voicer(args = ("voicer",), setup = setup)]
fn process(harness: Option<OpHarness>) -> f32 {
    black_box(harness.map_or(0.0, OpHarness::render))
}

library_benchmark_group!(name = micro; benchmarks = process);
// iai clears the environment before each bench for determinism, which would strip the gate's skip
// list. Pass exactly that one var through so [`setup`] can see it (see [`setup`] for why we skip).
main!(
    config = LibraryBenchmarkConfig::default().pass_through_env("REUBEN_MICRO_BENCH_SKIP");
    library_benchmark_groups = micro
);
