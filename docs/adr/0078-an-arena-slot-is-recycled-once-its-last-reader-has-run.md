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

Bundled instruments hold 19–72% fewer arena buffers, and the voice patches — the ones multiplied by
voice count — hold 0–52% fewer:

```
  acid-techno      50 -> 20      acid-pad-voice   21 -> 10
  euclidean-drums  53 -> 15      kick-voice       10 -> 6
  groovebox        25 -> 13      snare-voice      10 -> 5
  breath-harp      20 -> 11      default-voice    10 -> 7
  chord-player     12 -> 9       acid-bass-voice  14 -> 11
  strum-harp       16 -> 13      chord-player     7 -> 5
```

A serial chain now costs the same two slots at any depth, which is the property worth stating: the
arena is sized by the graph's *width* at its widest point, not by its size. A wide flat graph saves
nothing, and that is correct — its edges really are all live at once.

Render cost is unchanged by construction — the same number of fills, over fewer distinct buffers —
and measures so: 0.03–0.09% Ir across the macro cases, noise. Construction pays 0.9–2.2% for the
liveness pass, under the gate's 3% warn band, and stays linear (2.005–2.032× per doubling): the pass
is one walk of the already-built adjacency index and the free list is a `Vec` push/pop. Getting
there took one non-obvious shape — the expiry buckets are an intrusive linked list over a flat
array rather than a `Vec` per node, because a heap allocation per node on the build path is worth
1.4% on the deep shape all by itself. That is the construct gate doing the job ADR-0077 built it
for, on the first change after it landed.

**Slot assignment is deterministic but not stable.** Reuse is LIFO over a free list filled in
execution order, so a given graph always produces the same assignment — but a graph edit that shifts
the topological order shifts which slot a port lands on. Nothing may key on a slot index across
Swaps; survivor transplant already matches on node identity, not buffer index.

The pass is optimal for a chain and merely good in general — it is a linear scan over one fixed
topological order, not an interference-graph colouring, and it cannot reorder independent branches
to shorten a live range. A parallel executor free to reorder branches would need the pinning
question re-asked, since "last reader" would stop being a single index.
