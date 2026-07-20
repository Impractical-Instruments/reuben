# Why: The runtime artifact is the Plan, whose lifecycle is Build then Swap-and-Render, and the first build is just a Swap from the empty Plan.

[Rule](../../execution-runtime.md#plan-lifecycle)

The system needed precise vocabulary for *when* things happen — especially what is allocated when —
and the key realization is that the first graph build and every later live edit are the **same
operation**. So there are three phases: **Build** (compile the binary; operator *types* exist, no
user content, no pools), **Swap** (the single runtime transition that changes the graph —
off-thread **Instantiate** of a new Plan, atomic install at a block boundary, survivor-state
migration, deferred reclaim), and **Render** (execute the current Plan per block, hard realtime).

Making the first build "a Swap whose predecessor is the empty Plan" (zero operators, renders
silence) is the load-bearing move: state migration finds no survivors, reclaim finds nothing to
free, and the atomic install is identical to every other Swap — there is **no special cold-start
code path**, so the startup path is continuously exercised by every edit. "Bounded at creation
time" therefore means bounded at Instantiate, a runtime, off-RT event: resizing a voice pool is
just another Swap. **Instantiate** is kept as a sub-term (the off-thread construction, the first
half of a Swap), not a top-level phase. Engine start/stop — acquiring the audio device and running
the Render thread — is deliberately *orthogonal* to Plans and brackets the whole lifecycle rather
than folding into Swap.

The corollary invariant lives in its own rule: Render never allocates because all allocation is
pushed into Swap ([render-is-allocation-free](render-is-allocation-free.md)). The mechanics of what
the Swap *vessel* is, and how survivors carry state, are
[engine-swap-unit](engine-swap-unit.md) and [survivor-migration](survivor-migration.md).

Distilled from: ADR-0009
