# Why: A channel-bound signal input pipe reads the logical input master only at the top level; nested bindings are inert and unsupplied channels dark-degrade.

[Rule](../../composition-operators.md#logical-input-master)

Live input enters the graph through the same mechanism everything else does — [an interface
pipe](interface-pipes.md) — rather than through a special input operator. A top-level signal input
pipe with a `channel` binding reads the **logical input master**: the caller hands the renderer one
buffer per logical input channel, and each bound pipe copies its channel. It is the exact dual of
the output master, which is what keeps N-channel input from needing any concept the output side does
not already have. Fan-out works the way output broadcast does: two pipes may bind the same channel.

The rule exists for the two cases where the obvious reading is wrong.

**Only the top level reads the master.** A channel binding on a pipe inside a subpatch, or inside a
Voicer-hosted voice instrument, is *inert* — the pipe is fed by its parent edge or its host, and the
binding does nothing. This has to be a stated rule because the alternative is seductive and
incoherent: if a nested pipe read the master directly, every voice of a polyphonic instrument would
receive the same live input independently of how the patch wired it, and the parent's own wiring
into that pipe would be silently ignored or silently summed. Inert-plus-a-load-warning is the
honest answer — the binding is visible in the document, so the loader says out loud that it will not
do anything rather than leaving the author to infer it from silence.

**An unsupplied channel dark-degrades rather than failing.** The caller may hand over fewer buffers
than the patch binds, or a short buffer; a device may not have the channel at all. The pipe falls
back to its declared default — zeros for a bare pipe — and a short buffer's tail reads zeros. The
pipe stays message-drivable throughout, which is what makes the degrade genuinely soft: a patch
built around an input that is not present still loads, still plays, and can still be driven from
elsewhere. This is the same posture the [host shell](../host-shell-io/degradation-is-fixed-and-counted.md)
takes at its own edges, applied one layer in.

Live input is the sanctioned exception to [deterministic render](../execution-runtime/deterministic-render.md),
and it is scoped so that a patch with no input pipes gains no new nondeterminism at all. Offline
render closes the loop by injecting known buffers into the same pipes, so a rendered file is
bit-reproducible even though the live path is not.

Decided in: issue #639 — settled directly, no ADR.
