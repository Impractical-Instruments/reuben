# ADR-0078 — an arena slot is recycled once its last reader has run, and anything read off the end of the schedule says so

Overturns no rule. The edge-buffer arena's *sizing* was never stated in `docs/rules/`; this is the
first decision to pin it down, so it lands here to be folded into
[execution & runtime](../rules/execution-runtime.md) on the next absorb.

## Context

Instantiate handed out arena buffer indices from a counter that only ever went up: every declared
Signal output and every materialized Value→Signal edge got a permanent slot for the life of the
Plan. There was no liveness analysis, so a slot stayed allocated long after its last consumer had
run. A deep serial chain — osc → filter → shaper → amp, the shape most instruments actually are —
allocated one buffer per stage while only two were ever live at a point in the topological order.

Each slot is `block_size × f32`: 512 B at the shipped 128-frame block. Multiplied per Plan in the
Renderer, and again **per voice** inside the Voicer, which instantiates a full sub-Plan and its own
arena for every voice in the pool. The buffer count is the multiplicand of that product, and it was
being set by how many edges a document *has* rather than how many are *simultaneously live*.

This surfaced while sizing the wasm heap for the browser player, where per-node footprint is a
budget rather than an accounting curiosity.

The seam was already favourable. Buffer assignment is centralized in `Plan::instantiate`; arena
slots are opaque indices to every reader; `num_buffers` is the single number every arena allocation
is sized from. Nothing had to move for a liveness pass to fit — the assignment loop only had to run
in execution order, which it did not, because it predated needing to.

## Decision

**Instantiate computes each Buffer output's last reader and returns its arena slot to a free list
once that reader has run** — linear-scan register allocation over the topological order it already
computes. `num_buffers` falls out the same way it always did; it is just a smaller number.

Three things decide whether a slot may be recycled, and all three are consequences of *who reads
the buffer*, not of what produced it:

**A reader inside the schedule dates the slot.** A port's expiry is the execution index of its last
consumer, or the producer's own index when nothing reads it (a dead branch returns its slot
immediately). Expiry is applied **after** the expiring node is built, never before — which is what
keeps a node from being handed a slot it is itself still reading, and so keeps `process_node`'s
output swap (a `mem::take` of the output buffers out of the arena) disjoint from its inputs.

**A reader off the end of the schedule pins the slot forever.** A master tap is summed after every
node has run; a Signal `interface` output is read by a *host* — the Voicer reads a voice's `audio`
out of the arena after `render_plan` returns, and a host that binds graphs directly supplies that
boundary with no master tap to pin it a second time. Both are excluded from the liveness table
outright. This is the clause a future change is most likely to break: **a new post-schedule reader
of an arena buffer must pin its slot, or it silently reads whatever later node inherited it.** It
does not fail loudly, it plays the wrong audio.

**A slot that carries state across the block boundary is never in the pool at all.** Materialize
scratch holds an input's zero-order-hold value between blocks and is excluded from the per-block
edge clear precisely so it persists; recycling one would hand an input's held value to a different
input. Scratch is allocated outside the free list and never returned to it.

**A recycled slot is re-zeroed immediately before its new producer runs.** The per-block edge clear
runs before any node does, so it leaves the block's *first* producer of a slot a fresh buffer and
nobody else. An operator owns the samples it writes and the engine's clear is what defines the rest
as silence — so without the re-zero, an operator that writes only part of its output would read the
previous edge's audio in the gap. The total zeroing across a block is unchanged either way: one
fill per signal output port, whether those ports hold distinct slots or share them.

## Consequences

Bundled instruments hold up to 61% fewer arena buffers, and the voice patches — the ones
multiplied by voice count — up to 52% fewer:

```
  euclidean-drums  62 -> 24      acid-pad-voice        21 -> 10
  acid-techno      50 -> 20      snare-voice           10 -> 5
  groovebox        25 -> 13      kick-voice            10 -> 6
  breath-harp      20 -> 11      openhat-voice         10 -> 6
  strum-harp       16 -> 13      default-voice         10 -> 7
  chord-player     12 -> 9       hat-voice              9 -> 6
  mic-space         6 -> 5       acid-bass-voice       14 -> 11
  default           2 -> 2       chord-player-voice     7 -> 5
                                 shaped-vca             5 -> 5
                                 strum-harp-voice       3 -> 3
```

Two documents save nothing, and both are honest: `default` is two nodes, and `shaped-vca` and
`strum-harp-voice` are wide and shallow — every edge really is live at once. Measuring this needs
care: a resource that fails to resolve degrades the load to a *smaller* graph rather than failing,
so a harness with an incomplete resolver reports a smaller number on both sides and a wrong delta.
The first draft of this table did exactly that on `euclidean-drums` (reading 53 -> 15 off a
147-node document loaded as 111 nodes). The numbers above come from a run that treats any
resource-resolution warning as fatal — the same trap `benches/common/construct.rs` guards against
by asserting its own node count.

A serial chain now costs the same two slots at any depth, which is the property worth stating: the
arena is sized by the graph's *width* at its widest point, not by its size. A wide flat graph saves
nothing, and that is correct — its edges really are all live at once.

Render does the same number of fills over fewer distinct buffers, and measures as noise: every
macro case lands inside ±0.2% Ir, and neither the magnitude nor the sign repeats across runs. It is worth being exact about what that does and
does not show. Instruction count is not the axis this change is meant to move — the point is
resident bytes, and the locality win from a hot arena that fits in cache is invisible to callgrind.
The Ir number's job here is only to establish that the re-zero loop and the extra indirection cost
nothing measurable; it is not evidence of a speedup, and none is claimed.

Construction pays 1.2–3.1% for the liveness pass — inside the gate's 10% fail threshold, with the
three widest cases in its 3% warn band — and stays linear (2.006–2.040× per doubling): the pass is
one walk of the already-built adjacency index and the free list is a `Vec` push/pop. Getting there
took one non-obvious shape: the expiry buckets are an intrusive linked list over a flat array
rather than a `Vec` per node, because a heap allocation per node on the build path is worth ~1.4%
on the deep shape all by itself. That is the construct gate doing the job ADR-0077 built it for, on
the first change after it landed.

**Slot assignment is deterministic but not stable.** Reuse is LIFO over a free list filled in
execution order, so a given graph always produces the same assignment — but a graph edit that
shifts the topological order shifts which slot a port lands on. Nothing may key on a slot index
across Swaps; survivor transplant already matches on node identity, not buffer index.

One place keys a slot index across *plans*, and it predates this change: the Voicer resolves
`audio`'s arena index from voice 0 and uses it against every voice's arena. The voice graphs are
built independently (a `Graph` is not `Clone`), so this rests on Instantiate being a pure function
of the graph — which holds, and is verified stable across every bundled patch. But the index used
to be "the Nth buffer port in insertion order" and is now the output of a liveness pass, a quieter
function of the same input, and the failure mode is the wrong voice's audio rather than a crash.
So `on_instantiate` now `debug_assert!`s that every voice plan agrees. The assumption did not
change; what changed is that it is now written down and checked.

**An `Executor` may not reorder nodes.** Liveness is dated against the one linearization
Instantiate produced, so two independent branches can share a slot and running one where the other
was planned makes them overwrite each other. The `Executor` trait doc now says so; before this
change any valid topological order was safe, and that freedom is gone.

The pass is optimal for a chain and merely good in general — it is a linear scan over one fixed
topological order, not an interference-graph colouring, and it cannot reorder independent branches
to shorten a live range. A parallel executor free to reorder branches would need the pinning
question re-asked, since "last reader" would stop being a single index.
