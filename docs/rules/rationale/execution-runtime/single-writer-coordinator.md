# Why: A single-writer Coordinator owns all graph structure, and everything else crosses the RT boundary to a read-only Render by lock-free message passing.

[Rule](../../execution-runtime.md#single-writer-coordinator)

The outside world — audio device, OSC/MIDI/Link adapters, GUI, agents — must interact with the
realtime Render core without ever causing it to allocate, block, or see torn state. The answer is
three regions with one crisp invariant: **one writer of structure (the Coordinator), one reader at
Render (an immutable Plan), everything else lock-free message passing.** The Coordinator is the
single non-RT writer — it owns the canonical graph and the instrument library, receives edit
commands, performs Instantiate + [Swap](plan-lifecycle.md), and runs deferred reclaim. Render only
*reads* the current Plan.

Everything crosses the RT boundary by lock-free queue, split by consequence: params/control on a
Message queue Render drains each block (a knob turn never reaches the Coordinator — no Swap);
structural edits on a command queue to the Coordinator, which instantiates a new Plan and hands it
to Render for the atomic install, then reclaims the retired Plan off-thread; and a Render→outside
queue for metering, levels, emitted Messages, and introspection — so agents observe a live system
*without reaching into Render*. No shared mutable state crosses the boundary, a discipline Rust's
`Send`/`Sync` enforces. This same boundary is the **removable-native-layer line**: the I/O region
and the executor pool are native and removable; the Render core and Coordinator are portable, so an
embedder swaps the device callback for its tick and the worker pool for its job system while the
Coordinator and queues are unchanged ([embed-surface](embed-surface.md),
[pluggable-executor](pluggable-executor.md)). The Coordinator is a passive, OS-free
`reuben_core::coordinator` — no clock, no threads, no I/O — and single-writer discipline is enforced
simply by `&mut self`; the native shell holds it behind one mutex so an `expect`-compare → swap →
publish → reclaim runs as a single critical section (a correct compare-and-swap). The lock-free
boundary is tuned for the RT reader: the crossing slots are cache-line padded so a Coordinator
reclaim poll does not bounce exclusive ownership against the callback's own atomics, and a polling
drain *peeks* before it swaps so an empty poll never issues the exclusive-ownership operation that
would steal the slot's cache line from the filling thread on every miss.

Distilled from: ADR-0012
