# Why: A host serving the structure channel fills one seam for what only a host can know — what a path resolves to, where a control batch goes, what the counters read, how the device map is republished after a swap, and when the retired Engine may be freed — and the window decides everything else the verbs mean.

[Rule](../../execution-runtime.md#engine-host-seam)

Once [both ends of the channel are the window's](../agent-mcp/structure-channel-envelope.md), the
question left is what a host still has to supply, and the answer is the smallest one that works: what
only a host can *know*. A path resolves against a store the host owns. A control batch goes to an
ingress the host built. The counters read whatever the host is counting. The device output map has to
be republished after a swap, and only the host knows what the devices are. And the deferred free waits
for the retired Engine to come home — expressed as a gate the **host returns**, not as the Coordinator
handed back to it, so the reclaim protocol is not something a second host re-derives.

Everything else stays the window's, because everything else is a decision about what a verb *means*.
The Coordinator is single-writer, so it sits behind one lock and concurrent connections serialize on
it; a swap validates and builds a whole new Engine off-thread and fills the install mailbox, and the
answer is "queued, not applied"; `send` is the one verb that does not touch the Coordinator at all,
because control is not structure.

**What the seam buys is what the previous shape allowed.** A host serving the channel used to write a
dispatch loop, and a dispatch loop can reimplement the guard, the batch bounds and the report — badly,
differently, and stay green. Writing a seam implementation is more type to satisfy up front and
materially less to get right.

What stays on the host's side of it is the whole host-shell layer: the listener, the threads, the
device map, the render-callback liveness gate. None of those are things the window could hold without
becoming a host itself.

Distilled from: ADR-0071
