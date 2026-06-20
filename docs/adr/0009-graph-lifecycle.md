# Graph lifecycle: Build → Swap ⇄ Render, over a Plan

## Context

ADR-0001 established a static parallel schedule recomputed when the graph changes, and the need for lock-free live mutation with per-operator state preserved across recompiles (Q4). We need precise vocabulary for *when* things happen — especially what is fixed/allocated when — and we found that the first graph build and every subsequent live edit are the same operation.

## Decision

The runtime artifact is the **Plan**: the static parallel execution schedule (topologically ordered, clustered for parallelism — ADR-0001).

Three phases:

- **Build** — compile the engine binary (`cargo build`, dev-time). Operator *types* exist; no user content, no pools. Voice-pool sizes and graph bounds are *not* fixed here.
- **Swap** — the single runtime transition that changes the graph. Off the audio thread: **Instantiate** a new Plan from the graph description (topo sort, cluster, allocate the delta — new operators, voice pools, edge buffers). Then atomically install it as the live Plan at a block boundary, **migrate survivors' state** (operators present in both old and new Plan, matched by stable identity), and **reclaim** the old Plan and removed operators (deferred, off-thread). All allocation lives here.
- **Render** — execute the current Plan per block on the audio thread. Hard realtime, allocation-free. Playing a note, turning a knob, etc. happen here against already-allocated resources.

**The first graph build is just a Swap whose predecessor is the empty Plan** (zero operators, renders silence). State migration finds no survivors (all cold), reclaim finds nothing to free, and the atomic install is identical to every other Swap. There is no special "cold start" code path.

**Instantiate** is retained as a sub-term: the off-thread *construction* of a Plan — the first half of every Swap. It is not a top-level phase.

**Engine start/stop** — acquiring the audio device and running the Render thread — is orthogonal to Plans (it governs whether the audio callback runs at all) and brackets the whole lifecycle. It is deliberately *not* folded into Swap.

## Consequences

- One code path for first-build and live-edit → the startup path is continuously exercised by every edit.
- "Bounded at creation time" means bounded at **Instantiate** (inside a Swap) — a runtime, off-RT-thread event. Resizing a voice pool or any structural change is just another Swap.
- Render never allocates; all allocation is pushed into Swap.
