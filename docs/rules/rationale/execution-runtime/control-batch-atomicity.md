# Why: A control gesture crosses into the engine as one bounded batch pushed in a single unit, so it never straddles a block, never lands half-applied, and never exceeds the size bound Render's deadline requires.

[Rule](../../execution-runtime.md#control-batch-atomicity)

An authoring gesture is multi-control — one move touches several addresses — so the door verb is a
batch, not a message. Per-message would mean N connections and N engine-side threads to replace what
one socket dispatched in a loop, and it would split one gesture into N independently-failable steps.

Batching only earns its atomicity claim if the batch stays whole **past** the exchange, so the ingress
into the render callback carries the batch, not the message, and it is pushed **once**. That single
push is what makes three promises true rather than aspirational: concurrent handler threads cannot
interleave into each other's gestures, the callback cannot apply half a gesture to one block and half
to the next, and there is no partial-failure window where some messages are queued but the client is
told the batch failed. The external OSC producer gets the same treatment, which incidentally fixes a
latent oddity — an OSC **bundle** means "these are simultaneous", and its messages used to be pushed
one at a time and could straddle a block.

A batch therefore lands in a **single render callback**, which makes its size an RT property rather
than a request-size preference: unbounded, one authoring gesture could blow a render deadline, and
[render-is-allocation-free](render-is-allocation-free.md) is not negotiable. UDP inherited this bound
for free from the kernel receive buffer; a channel that does not have a kernel buffer in front of it
has to state it. The cap is generous against real gestures (a move is a handful of controls; recalling
a whole surface is on the order of a hundred) and is shared between the door advertising it and the
engine enforcing it so the two cannot drift — the engine enforces regardless, since a door is a
courtesy and the channel is the contract.

Two shapes fall out of atomicity. The ack is a **unit**, not a count: the batch is queued as one unit,
so no partial outcome exists for a count to describe, and every rejecting path answers an error
instead. A field that can hold exactly one value is not information — it invites a client to read
`count < len` as "some were rejected", a signal that can never arrive. And an **empty** batch is
refused, for a different reason: acking a no-op as success would let a client bug that drops its
messages read as a working send. "Queued", not "applied", throughout — an address routing to no
node/port is dropped at the ingress, exactly as a stale external datagram is.

Distilled from: ADR-0065. The RT justification for the batch bound was harvested from the
`coordinator::wire` constant, where it also lives as mechanics.
