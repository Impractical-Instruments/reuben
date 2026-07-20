# Why: The render hot path is guarded by an instruction-count perf gate that diffs HEAD against its base ref and fails a PR on a >10% regression, with wall-clock benchmarking left to local runs.

[Rule](../../web-product-process.md#perf-benchmark-gate)

`render_block` is the hot loop — every operator's `process` runs under it once per block for the life
of a stream — and there was no way to stop a change silently making it slower. The goal is narrow:
catch *major* regressions cheaply, without a flaky gate that cries wolf. Two facts decide the design.
Wall-clock timing on CI's shared runner jitters ±10–30%, so a naive `cargo bench` gate false-fails
constantly — so wall-clock (criterion) stays **local only**, answering "how many ×realtime". Callgrind
**instruction counts don't jitter**: independent of CPU speed and of what else the runner is doing, the
same code yields the same count — so the CI gate is iai-callgrind, and it is the only perf job that
gates. They measure different things on purpose; neither replaces the other.

Instruction counts *do* move with the toolchain, which CI floats — so the gate never commits a
baseline numbers file (it would churn on every rustc release). Instead it **benches HEAD and its base
ref with the same runner toolchain and diffs them**, so toolchain drift cancels; the baseline is the
PR's base branch or the previous commit. The benched unit is end-to-end `render_block` of real
instruments spanning the heavy operator families, driven by one fixed deterministic schedule (no clock
reads, no RNG) so the counts are byte-stable and both layers measure identical work. The verdict is
two-tier — **warn > 3%, fail > 10%** — where 10% is "major" (an accidental O(n²), a clone or alloc in
the per-sample loop) and 3% surfaces slow creep without blocking; the hard fail is enforced by
callgrind itself, the rest parsed best-effort so a schema shift degrades gracefully. A separate
`main`/`dev` orphan-branch trend (`bench-history`) persists the absolute numbers as JSONL with a
self-rendered dashboard, so a regression is visible before it lands rather than only in a job summary
that ages out.

One methodology pin is load-bearing: benchmarks build at `[profile.bench] codegen-units = 1`. At the
default 16 CGUs, merely adding *cold* source to `reuben-core` repartitions LLVM's codegen units and
re-rolls register allocation for each operator's vtable-dispatched `process`, swinging its callgrind
`Ir` by ±13% with **zero** change in actual work — the gate would read codegen-partition luck, not the
render path. CGU=1 collapses those false deltas to 0.00% symmetrically while real extra work stays
CGU-invariant and still fires. This is a *fixed measurement methodology*, not a perf lever under test,
and it is a different flag-for-a-different-reason than the release-side
[static-link CGU pin](static-link-operator-registration.md); `[profile.release]` deliberately stays at
the default so shipped binaries keep parallel codegen.

Distilled from: ADR-0019
