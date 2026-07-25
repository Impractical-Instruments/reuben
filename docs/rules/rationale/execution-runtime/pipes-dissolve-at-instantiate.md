# Why: Pass-through interface pipes are collapsed out of the schedule at Instantiate.

[Rule](../../execution-runtime.md#pipes-dissolve-at-instantiate)

An [interface pipe](../composition-operators/interface-pipes.md) is an authoring and format concept:
a named boundary entry that mints an address, so a graph can expose a surface without its internals
leaking. It is not a piece of DSP. Nothing about the sound depends on a pipe existing, and a patch
whose pipes were manually flattened away would sound identical.

Rendering one as a real node is therefore pure overhead — and not a small constant. Every node in
the schedule costs a full per-block engine pass: routing, segmenting, and for a signal pipe an arena
buffer plus a copy. That is paid per block, and then **multiplied by the Voicer's per-voice plans**,
so a single pipe inside a voice instrument becomes one wasted node per voice per block. A design
that encourages authors to expose a clean interface must not charge them for doing it, or the
performance-conscious answer becomes "expose less," which is exactly backwards.

So Instantiate removes each dissolvable pipe and rewires around it, and the rendered schedule
becomes what a hand-flattened patch would have been. The rewiring has two shapes because the pipe
has two ways of being fed. When a wire feeds it, the pipe's single consumer takes that feeder wire
directly — genuinely zero-copy for signals, since the buffer the producer already writes is the one
the consumer already reads. When the *boundary* feeds it by message — a Voicer, or external OSC —
there is no internal producer to point at, so the consumer's own materialize/latch path takes over,
and the pipe's rest seed is transferred to it as a value-override so an unfed pipe still starts at
the value the author declared.

That second case is why dissolution is a **behavior-preserving** transform rather than a deletion:
the pipe carries state (its declared default, its rest value) that has to land somewhere, and the
pass moves it rather than dropping it. Where it cannot — where a pipe's removal would change what
the graph does — the pipe stays a node. Keeping those exceptions is what makes the optimization safe
to apply without the author having to know it happens.

Doing this at Instantiate rather than at load keeps the **document** honest: the canonical graph the
Coordinator owns still has the pipes in it, so the document round-trips, the projection shows the
interface the author wrote, and only the runtime artifact is flattened. Optimization belongs to
[the Plan](plan-lifecycle.md), not to the source of truth.

Decided in: issue #639 — settled directly, no ADR.
