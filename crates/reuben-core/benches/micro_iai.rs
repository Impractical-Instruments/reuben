//! CI regression gate, micro layer (#30): deterministic instruction count of each operator's
//! `process` over the fixed 1 s schedule, via callgrind. Same rationale as `macro_iai` — counts are
//! CPU-independent and byte-stable, so the same-toolchain compare in CI flags a per-operator
//! regression without wall-clock flake. The macro layer catches a graph getting slower;
//! this layer says which operator.
//!
//! The [`setup`] wrapper builds the harness (alloc, resource decode, event construction) outside the
//! measured region; only the render loop is counted. It returns `None` for a kind the CI perf gate
//! asked to skip (a PR-new operator with no baseline — see [`setup`]), so that operator is dropped
//! from the comparison instead of crashing the baseline run.
//!
//! iai's `#[bench::…]` list is compile-time, so it can't iterate the registry (which is
//! `inventory`-collected, and so only readable at runtime). **This file is therefore the one
//! hand-maintained census of benched operators** — one `id => "kind"` line each, expanded into the
//! attributes by [`micro_bench_ops!`].
//!
//! It lives *here*, in the never-swapped bench harness, rather than in `bench_support`, for the same
//! reason the skip list does (see [`setup`]): the perf gate swaps `reuben-core/src` to the baseline
//! ref, so a census in `bench_support` would be swapped out from under the run.
//!
//! The #30 forcing function (`bench_support::tests::iai_list_covers_every_workload`, in the `check`
//! job) reads the list back out of this file's source with `include_str!` and asserts it equals
//! `WORKLOADS` — so adding an operator still reds CI until it is benched, but the census that used
//! to be duplicated into a `MICRO_IAI_KINDS` const is now read from the one place it is written.

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

/// Expand the operator census into the `#[library_benchmark]` fn and its `#[bench::<id>]` cases.
///
/// One line per benched operator, `<bench id> => "<operator kind>"`. The **id is not derived from
/// the kind**: iai names each callgrind case by its id, and the CI perf gate matches HEAD against
/// the baseline by that name — so renaming an id unmatches that operator's history and silently
/// drops it from the gate (the #104 masking bug, in slower motion). The ids are therefore carried
/// verbatim from when each bench was added, however inconsistent they look beside their kinds.
macro_rules! micro_bench_ops {
    ($($id:ident => $kind:literal),* $(,)?) => {
        #[library_benchmark]
        $(#[bench::$id(args = ($kind,), setup = setup)])*
        fn process(harness: Option<OpHarness>) -> f32 {
            black_box(harness.map_or(0.0, OpHarness::render))
        }
    };
}

// The census: every benched operator, plus the bench-only `overhead` case. Keep alphabetical by
// kind, for diffing against the registry's stable order.
micro_bench_ops! {
    abs              => "abs_f32_signal",
    abs_value        => "abs_f32_value",
    abs_i32_value    => "abs_i32_value",
    add              => "add_f32_signal",
    add_value        => "add_f32_value",
    add_i32_value    => "add_i32_value",
    ceil_to_i32      => "ceil_f32_i32_value",
    ceil             => "ceil_f32_signal",
    ceil_value       => "ceil_f32_value",
    chord            => "chord",
    clamp            => "clamp_f32_signal",
    clamp_value      => "clamp_f32_value",
    clamp_i32_value  => "clamp_i32_value",
    clock            => "clock",
    compressor       => "compressor",
    delay            => "delay",
    differentiate    => "differentiate_f32_signal",
    div              => "div_f32_signal",
    div_value        => "div_f32_value",
    div_i32_value    => "div_i32_value",
    djfilter         => "djfilter",
    envelope         => "envelope",
    euclid           => "euclid",
    filter           => "filter",
    floor_to_i32     => "floor_f32_i32_value",
    floor            => "floor_f32_signal",
    floor_value      => "floor_f32_value",
    granulator       => "granulator",
    harmony          => "harmony",
    integrate        => "integrate_f32_signal",
    lfo              => "lfo",
    m2s              => "m2s",
    map              => "map_f32_signal",
    map_value        => "map_f32_value",
    max              => "max_f32_signal",
    max_value        => "max_f32_value",
    max_i32_value    => "max_i32_value",
    min              => "min_f32_signal",
    min_value        => "min_f32_value",
    min_i32_value    => "min_i32_value",
    modulo           => "modulo_f32_signal",
    modulo_value     => "modulo_f32_value",
    modulo_i32_value => "modulo_i32_value",
    mul              => "mul_f32_signal",
    mul_value        => "mul_f32_value",
    mul_i32_value    => "mul_i32_value",
    negate           => "negate_f32_signal",
    negate_value     => "negate_f32_value",
    negate_i32_value => "negate_i32_value",
    noise            => "noise",
    osc_out          => "osc_out",
    oscillator       => "oscillator",
    output           => "output",
    overhead         => "overhead",
    pan              => "pan",
    pitch2freq       => "pitch2freq",
    power            => "power_f32_signal",
    power_value      => "power_f32_value",
    reciprocal       => "reciprocal_f32_signal",
    reciprocal_value => "reciprocal_f32_value",
    resonator        => "resonator",
    reverb           => "reverb",
    round_to_i32     => "round_f32_i32_value",
    round            => "round_f32_signal",
    round_value      => "round_f32_value",
    sample           => "sample",
    saturator        => "saturator",
    sequencer        => "sequencer",
    snap             => "snap",
    strum            => "strum",
    sub              => "sub_f32_signal",
    sub_value        => "sub_f32_value",
    sub_i32_value    => "sub_i32_value",
    subpatch         => "subpatch",
    transpose        => "transpose",
    trunc_to_i32     => "trunc_f32_i32_value",
    trunc            => "trunc_f32_signal",
    trunc_value      => "trunc_f32_value",
    unpack_note      => "unpack_note",
    voicer           => "voicer",
}

library_benchmark_group!(name = micro; benchmarks = process);
// iai clears the environment before each bench for determinism, which would strip the gate's skip
// list. Pass exactly that one var through so [`setup`] can see it (see [`setup`] for why we skip).
main!(
    config = LibraryBenchmarkConfig::default().pass_through_env("REUBEN_MICRO_BENCH_SKIP");
    library_benchmark_groups = micro
);
