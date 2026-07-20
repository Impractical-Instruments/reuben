# Why: The core emits a task-and-dependency execution plan and dispatches it through an executor interface rather than owning threads.

[Rule](../../execution-runtime.md#pluggable-executor)

reuben must embed in game engines, DAWs, and a browser worklet, each of which already owns a job
system or a single callback thread. A core that owned its own thread pool would fight those hosts —
oversubscription, priority inversion, two schedulers contending. So the core stays a *producer of
work*: it emits the parallel execution plan (tasks plus dependency edges) and dispatches through an
executor interface. The standalone native layer ships a lock-free, pinned, allocation-free worker
pool; an embedded host maps the same tasks onto its own job system; a single-threaded host runs
them inline. The executor lives in the **removable** native layer, so the portable core carries no
threading policy — the same seam that makes the native I/O layer detachable
([embed-surface](embed-surface.md), [single-writer-coordinator](single-writer-coordinator.md)).

Because dispatch order is not fixed, the plan must still produce bit-identical output regardless of
how the executor interleaves tasks — see [deterministic-render](deterministic-render.md).

Distilled from: ADR-0001
