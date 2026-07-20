# Execution & runtime

> How the unified block graph is scheduled, threaded, swapped, and rendered in real time — the Plan lifecycle, RT boundary, determinism, latch service, and the embed surface.

## Now

reuben is one dataflow graph that mixes non-audio data (notes, chords, timing, gestures, OSC)
and audio, and runs it in real time. There is no separate control rate: sub-audio-rate control
travels as timestamped, OSC-shaped **Messages**, dense audio travels as float **Signal** buffers,
and both are computed together, one fixed-size **block** at a time, in a single dependency-ordered
pass. That order is a single static topological schedule, computed once when the graph changes and
coalesced into cost-weighted clusters so independent branches run concurrently. The core does not
own threads — it emits a task-and-dependency plan and hands it to a pluggable executor, so the same
graph runs under the native worker pool, a game engine's job system, or a WebAudio worklet. *(The
MVP ships a serial executor; the parallel executor is designed to slot in behind the same interface
and is not built yet.)* Output is bit-identical no matter how those tasks interleave; determinism is
a hard invariant, held by fixed fan-in order and a unit delay on real feedback cycles. The one
sanctioned exception is a boundary that is nondeterministic by nature — live audio input, like
OSC-in — so a patch with no input pipes gains no new nondeterminism, and offline render injects
known buffers into the input pipes to stay bit-reproducible. The whole stack is Rust exposing a
C ABI — chosen because its two hardest subsystems, lock-free plan swap and deterministic parallel
execution, are exactly what Rust checks at compile time.

The runtime artifact is the **Plan**: the immutable, already-allocated schedule the audio thread
executes. Its lifecycle is **Build → Swap ⇄ Render**. Render runs each block on the audio thread,
hard-realtime and allocation-free — it only ever reads the current Plan. Every change to the graph,
including the very first build, is a **Swap**: off the audio thread a single-writer **Coordinator**
instantiates a new Plan (topo sort, cluster, allocate the delta), the whole **Engine** vessel
crosses the RT boundary through a pair of single-slot atomic mailboxes, surviving operators keep
their state by pointer-transplant, and the retired Engine is reclaimed off-thread. Because the swap
is audibly abrupt, install is wrapped in a fixed ~20 ms master-gain duck. Nothing but the Coordinator
writes structure, and nothing crosses the RT boundary except by lock-free message passing — a shape
Rust's `Send`/`Sync` enforces — which is also the seam where the removable native I/O layer detaches
from the portable core and its **embed surface**, the `Engine` bridge shared by every host shell.

Inside a block, timing is sample-accurate without asking single-node authors to juggle sample
offsets: the engine holds a per-port zero-order-hold **latch** of each input's last Message, so a
follower just reads its current value as a constant, and a mid-block change takes effect at its exact
**frame** (float updates materialized into the buffer, held/event shapes block-sliced). Operators are
not merely sinks — they **emit** Messages over statically-wired typed edges, delivered downstream in
the same block in topological order through preallocated, allocation-free emit pools, so note data can
be re-processed (sequencer → voicer, transposers, tonal-context snap) rather than dead-ending as CV.

## Rules

<a id="unified-block-graph"></a>
### One unified graph of message and signal data runs in fixed-size blocks, each block computed in a single dependency-ordered pass.

[why](rationale/execution-runtime/unified-block-graph.md)

<a id="static-parallel-schedule"></a>
### Execution order is one static topological schedule, recomputed only when the graph changes and coalesced into cost-weighted clusters that run concurrently.

[why](rationale/execution-runtime/static-parallel-schedule.md)

<a id="deterministic-render"></a>
### Render output is bit-identical regardless of thread interleaving: fan-in sums in a fixed order and feedback cycles pay a unit delay.

[why](rationale/execution-runtime/deterministic-render.md)

<a id="pluggable-executor"></a>
### The core emits a task-and-dependency execution plan and dispatches it through an executor interface rather than owning threads.

[why](rationale/execution-runtime/pluggable-executor.md)

<a id="rust-core"></a>
### The core and native layer are written in Rust exposing a C ABI.

[why](rationale/execution-runtime/rust-core.md)

<a id="plan-lifecycle"></a>
### The runtime artifact is the Plan, whose lifecycle is Build then Swap-and-Render, and the first build is just a Swap from the empty Plan.

[why](rationale/execution-runtime/plan-lifecycle.md)

<a id="render-is-allocation-free"></a>
### Render only reads an immutable Plan and never allocates, frees, or blocks; all allocation lives off-thread in Swap.

[why](rationale/execution-runtime/render-is-allocation-free.md)

<a id="single-writer-coordinator"></a>
### A single-writer Coordinator owns all graph structure, and everything else crosses the RT boundary to a read-only Render by lock-free message passing.

[why](rationale/execution-runtime/single-writer-coordinator.md)

<a id="sample-accurate-timing"></a>
### A Message landing mid-block takes effect at its exact frame without operators tracking sample offsets.

[why](rationale/execution-runtime/sample-accurate-timing.md)

<a id="latch-service"></a>
### The engine holds a per-port zero-order-hold latch of each input's last Message so a follower reads its current value as a plain constant.

[why](rationale/execution-runtime/latch-service.md)

<a id="operator-message-emission"></a>
### Operators emit Messages over statically-wired typed edges, delivered downstream in the same block in topological order through preallocated emit pools.

[why](rationale/execution-runtime/operator-message-emission.md)

<a id="embed-surface"></a>
### The portable Engine bridge (queue_osc, fill, drain_outbound) lives in reuben-core as the one embed surface every host shell wraps.

[why](rationale/execution-runtime/embed-surface.md)

<a id="engine-swap-unit"></a>
### The unit of a Swap is the whole Engine, handed across the RT boundary through two single-slot atomic mailboxes with one swap in flight.

[why](rationale/execution-runtime/engine-swap-unit.md)

<a id="survivor-migration"></a>
### Operator state survives a Swap by pointer-transplanting boxes matched on fully-qualified address, operator type, and instantiate-time fingerprint.

[why](rationale/execution-runtime/survivor-migration.md)

<a id="swap-gain-ramp"></a>
### A live Swap is wrapped in a fixed engine-side master-gain ramp: fade to zero, install, fade back up.

[why](rationale/execution-runtime/swap-gain-ramp.md)

## Terms

- **Block** — the fixed-size processing quantum; each block computes message- and signal-domain data in one dependency-ordered pass.
- **Plan** — the runtime artifact: the immutable, already-allocated static parallel schedule (topo-ordered, clustered) that Render executes per block.
- **Instantiate** — the off-thread construction of a Plan (topo sort, cluster, allocate the delta); the first half of every Swap, where all allocation lives.
- **Swap** — the single off-thread transition that installs a new Plan/Engine at a block boundary, migrating survivor state and reclaiming the old vessel.
- **Render** — the hard-realtime, allocation-free per-block execution of the current Plan on the audio thread.
- **Coordinator** — the single non-RT writer of graph structure; owns the canonical graph and instrument library and performs every Swap.
- **Engine** — the portable bridge in reuben-core (`queue_osc` → `fill` → `drain_outbound`) a host shell drives, and the whole vessel (Plan + Renderer + scratch) that a Swap crosses.
- **Embed surface** — the portable rim of reuben-core (the `Engine` bridge) that each host shell wraps; the native I/O layer is the removable other side.
- **frame** — a sample offset within a block; the unit of sample-accurate Message timing.
- **latch** — the engine-held per-port zero-order-hold of an input's last Message, read by an operator as its constant current value.
- **survivor** — an operator that persists across a Swap (matched on address + type + instantiate-time fingerprint) and keeps its state via box transplant.
