# Why: Execution order is one static topological schedule, recomputed only when the graph changes and coalesced into cost-weighted clusters that run concurrently.

[Rule](../../execution-runtime.md#static-parallel-schedule)

Order is computed once, when the graph changes, not per block — the per-block hot path just walks
an already-decided schedule, which keeps Render cheap and predictable. The schedule is a DAG, so
independent branches are free to run concurrently; nodes are coalesced into **cost-weighted
clusters** so task-dispatch overhead is amortized instead of paying a per-node task. This is the
concrete artifact the rule calls the [Plan](../../execution-runtime.md#plan-lifecycle): a static
parallel execution schedule, topologically ordered and clustered.

Recomputing only on change is what pushes all the structural work off the audio thread and into
[Swap](../../execution-runtime.md#plan-lifecycle) — a knob turn never rebuilds the schedule, only
a structural edit does. Because the schedule is fixed between edits, the parallel layout is
stable and its determinism is checkable
([deterministic-render](deterministic-render.md)).

Distilled from: ADR-0001
