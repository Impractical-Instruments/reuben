# Why: Everything a block touches crosses the window as a re-export by identity, so nothing sits between the audio callback and the engine; the window owns a real function only where the cost is per session rather than per block.

[Rule](../../execution-runtime.md#render-half-is-re-exported)

The window's two halves have opposite cost profiles. Authoring is called from anywhere, off-thread,
with an irrelevant budget and every type serialized to a consumer. Render is called from the audio
callback, where a conversion per block *is* the cost. So [the window declaring its own
types](../agent-mcp/window-declares-its-own-types.md) stops at the render boundary: that decision was
argued about *wire* types — things serialized and advertised to a model — and a render handle is not
one. Declaring a window-owned render slot would buy no decoupling, because nothing about a render
handle is a serialization concern, and it would cost a conversion on every block.

The limit is written down rather than left to be inferred, because the failure mode is somebody
applying the earlier decision uniformly in good faith and discovering the cost as an instruction-count
regression with no record of why the boundary was ever drawn.

**The limit is about cost per block, not about the absence of functions**, and that is not a
contradiction. A host does not only *call* the render path; it has to **construct** one, and the
constructor needs a registry and a resolver. So the pair's constructor is a real function: it hides the
registry — the one argument a host would otherwise have to name the engine to supply, and there is one
operator set that a host does not choose — and it takes the window's own [resource
seam](../authoring-library/resource-seam-is-host-implemented.md), so a host implements one resolver
trait rather than two. An install happens once per session; a swap happens when an author swaps.
Everything a *block* touches — the Coordinator, the render side and slot, the audio config, Messages
and Args, the load warnings, the single-slot mailbox — is re-exported by identity, the same items, so
there is nothing between the callback and the engine to cost anything.

**The perf gate cannot see any of this, and saying so is the honest form of "no regression."** The gate
swaps the engine crates' source to the baseline and benches through the engine directly, so a change
confined to the window compiles identical engine source on both sides and the comparison is a no-op.
What protects the block here is that no wrapper exists on the per-block path — not the gate. A future
`#[inline]` passthrough would be equally invisible to it, which is worth knowing before one is
written.

Distilled from: ADR-0069, ADR-0072
