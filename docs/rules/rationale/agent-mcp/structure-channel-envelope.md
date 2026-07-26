# Why: Both ends of the structure channel are the window's — the verbs, their payloads, the framing, the default address and the batch bound — so what a swap means is written once rather than on whichever side happened to serve it.

[Rule](../../agent-mcp.md#structure-channel-envelope)

The engine half asks a question the authoring half never did. Its verbs do not run in the caller's
process: a request crosses a channel, and there is a **server on the other side of it**. Both ends are
doors — the native binary serves, the sidecar dials — so the wire between them belonged to neither,
and it was parked in the engine, a crate that serializes none of it, because that was the only place
both ends could see.

**Owning both ends is the point rather than a side effect.** A wire has two implementations by
construction, and while the envelope was homeless the answer to "what does a swap *mean*?" was written
on the serving side only, in the native crate, where a second host could not reach it: the `expect`
compare, the retain-prior report, the empty and over-long batch rejections, the "queued, not applied"
ack. Moving the envelope makes that meaning single-sourced the way the [document
verbs](document-verbs.md) already are, and leaves the host only [what only a host can
know](../execution-runtime/engine-host-seam.md).

The advertised roster moved with the verbs, for the same reason: a roster in a crate that advertises
nothing is a roster no door can be held to. With it at the window, the sidecar's manifest names the
window and nothing behind it.

**[Loopback-only](structure-channel-is-loopback-only.md) is untouched, and it is worth saying why.**
Structure edits are still strictly more powerful than OSC control, and one shared default address
still keeps server and client from drifting apart. Only the *address of the address* changed: not a
constant two doors both reach into the engine for, but one the window owns and both doors are handed.
A relocation that leaves a rule true is not an overturn.

One byte-neutral improvement fell out of the move and is the smallest possible demonstration of the
argument: the swap verb used to advertise a second, differently-wrapped copy of the diagnostic
description the other twenty-two verbs carried, because it was the engine's type and theirs was the
window's. There is one now.

Distilled from: ADR-0071
