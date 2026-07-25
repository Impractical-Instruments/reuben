# Why: A `process` block loop runs on flat locals — state copied in and written back once, port slices resolved before the loop.

[Rule](../../execution-runtime.md#block-loop-runs-on-locals)

Two abstractions that cost nothing per *block* cost real work per *sample*, and `process` is the
one place that distinction is measurable. Both are cases of the compiler being unable to prove
what a human can see by inspection.

**`&mut self` fields do not get promoted to registers.** A filter's integrator pair is two `f32`,
small enough to live in registers for a whole block — but ticking them through `&mut self` writes
both to memory every sample, because LLVM cannot prove the borrow is unaliased across the loop
body. Copying the state into a local and storing it back once after the loop drops `process` to
roughly one data-write per sample: the output store, and nothing else. This is why the raw DSP
components are **value-oriented** — a small `Copy` struct threaded through the block, not a `&mut
self` object — and the two halves are one decision: the type is `Copy` *so that* the loop can hold
it in registers, and the loop copies it *because* memory traffic per sample is the cost being
avoided.

**Port handles resolve through a table.** `io.read(IN_AUDIO)` re-derives its slice from the `Io`
input/output tables on every call — a table index plus an `Option` unwrap. Called once per block
that is free; called per sample it is a load the optimizer will not hoist, because the handle layer
is opaque to it. Binding flat locals before the loop restores the codegen the operators had before
handles existed. This one is worth stating explicitly because it is a *regression* the abstraction
introduced, not an inherent cost: handles were a correctness and ergonomics win, and the idiom is
what keeps them from being paid for in the hot path.

The convention is load-bearing across the operator set rather than a local trick — the block loop
is the innermost loop in the system, so a per-sample memory access there is multiplied by every
sample, every voice, and every block. `filter.rs` is the canonical worked example: it shows the
state copy, the resolved slices, and the held-unchanged fast path together on a real SVF.

The [perf gate](../web-product-process/perf-benchmark-gate.md) is what keeps this honest — an operator
rewritten to tick through `&mut self` shows up as an instruction-count regression rather than as a
review comment someone has to think to make.

Decided in: issue #639 — settled directly, no ADR.
