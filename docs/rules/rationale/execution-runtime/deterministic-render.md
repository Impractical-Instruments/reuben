# Why: Render output is bit-identical regardless of thread interleaving: fan-in sums in a fixed order and feedback cycles pay a unit delay.

[Rule](../../execution-runtime.md#deterministic-render)

Determinism is a hard invariant, not a nice-to-have: the graph runs across many cores under a
[pluggable executor](pluggable-executor.md), and if thread interleaving could change the output
the system would be untestable and unrepeatable. Two sources of non-determinism are closed
structurally. Floating-point summation is not associative, so **fan-in** (mixers, summing buses)
uses a fixed reduction order rather than whatever order tasks happen to finish. True **feedback
cycles** are broken with a unit delay — and only real cycles pay it, because the schedule is a
topological sort of a DAG and forward edges need no delay.

The same unit-delay rule is what lets operators emit Messages downstream in the same block while an
upstream (feedback) target waits a block
([operator-message-emission](operator-message-emission.md)). Determinism is also what makes
sample-accurate timing well-defined: slice points are a deterministic function of Message offsets
([sample-accurate-timing](sample-accurate-timing.md)).

Distilled from: ADR-0001
