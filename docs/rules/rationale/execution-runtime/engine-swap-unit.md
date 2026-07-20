# Why: The unit of a Swap is the whole Engine, handed across the RT boundary through two single-slot atomic mailboxes with one swap in flight.

[Rule](../../execution-runtime.md#engine-swap-unit)

"[Swap](plan-lifecycle.md) replaces the Plan" stays the conceptual truth, but *mechanically* the
whole **Engine** — Plan + Renderer + scratch — is what crosses, because the Plan is not
self-contained at runtime: the Renderer's edge-buffer arena is sized to the Plan, and the Engine's
scratch/channel counts are fixed at construction. Crossing a bare Plan would force the callback to
rebuild or resize those RT-side — allocation on the audio thread, or a preallocated-maximum regime
nothing else needs. Crossing the whole vessel means everything the callback touches post-install was
built before install, so [zero RT allocation](render-is-allocation-free.md) holds by construction.

The handoff is a pair of single-slot atomic mailboxes hand-rolled on `AtomicPtr` (in core, no new
dependency): an **install slot** the Coordinator fills and the callback drains, and a **retire
slot** the callback fills and the Coordinator drains. The Coordinator enforces **one swap in
flight** — it never installs the next Engine until the retired one returns — and times out into an
actionable error ("engine isn't consuming swaps; is audio running?") rather than queueing blind;
reclaim is the deferred off-thread free of the retired Engine. A lock-free SPSC queue crate was
rejected (a new dependency and a queue depth implying multiple in-flight swaps the single writer
never produces); ArcSwap/triple-buffering was rejected because the callback needs `&mut Engine` —
render mutates state — and reclaiming the old side without RT drops fights that shape.

Install happens once per swap, at a device block boundary. The callback *peeks* the install slot at
the **callback top** and begins the master-gain down-ramp there; the actual install lands later,
**when the ramp reaches zero** (the fade-to-zero install replaces the earlier install-at-the-callback-top — see
[swap-gain-ramp](swap-gain-ramp.md)). At that install, the retiring Engine's ≤1-block
rendered-but-unplayed residue (~5 ms) is discarded and its pending control Messages are dropped,
since a Message minted against the old Plan's port types may not be valid against the new one. That
~5 ms is below what the gain ramp papers over, so core-block-boundary precision inside the fill hot
loop was not worth the RT branching. The
Coordinator's RT counterpart (the slot that checks the install mailbox, runs the migration table,
posts the retiree) also lives in core, so both the native callback and the web worklet embed the
same machinery ([single-writer-coordinator](single-writer-coordinator.md)). Swaps never touch audio
devices — streams are fixed at `play` start; a swapped-in instrument that binds an absent input
dark-degrades to silence with a loud warning. Render-side swap cost is one atomic check per
callback, plus the migration table walk on the callback that installs. How survivors carry state is
[survivor-migration](survivor-migration.md).

Distilled from: ADR-0046
