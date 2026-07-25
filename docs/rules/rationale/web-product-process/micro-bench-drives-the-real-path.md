# Why: A per-operator micro-bench drives that operator's real per-sample path, not an early-out idle path.

[Rule](../../web-product-process.md#micro-bench-drives-the-real-path)

The [perf gate](perf-benchmark-gate.md) fails a PR on an instruction-count regression, which makes
each bench a *claim* that a number means something. The macro benches render a whole instrument, so
a regression there is real but unattributed. The micro layer drives one operator's `process`
directly — through the real engine, via [the one wiring
path](../composition-operators/single-io-wiring-path.md) — so a regression is attributable to the
operator that caused it.

Attribution is only worth anything if the bench exercises the path that actually costs. Operators
are full of legitimate early-outs, and a bench built from defaults walks straight into them: the
saturator's `tanh` has a libm early-out at zero, so benching it on silence measures the branch rather
than the transcendental. The compressor's detector idles below threshold, and a held or DC key is
removed by the key high-pass, so only an above-threshold AC key runs the real peak-log, ballistics,
and makeup-exp path. An envelope with no gate idles at zero. In each case the bench is *green, fast,
and meaningless* — and worse than meaningless, because a real regression in the expensive path would
not move the number, so the gate reports safety it cannot deliver.

So each operator's bench declares the **minimum** input that reaches its real per-sample work, and
no more. Minimality is the other half: a bench that drives every input to be safe measures setup and
control handling that the operator would not do in a realistic patch, which makes the number noisy
and its regressions hard to read. The recipe is chosen per operator because "what counts as real
work" is a property of the DSP, not something a generic harness can infer.

The census of what is benched is one source rather than a convention. Tests hold it in sync with the
registry and with the CI scan, so an operator added without a bench is a **failure** rather than a
silent gap — the failure mode this guards against being a hot-path operator that was never measured
and therefore never regressed, which reads identically to one that is fast.

Decided in: issue #639 — settled directly, no ADR.
