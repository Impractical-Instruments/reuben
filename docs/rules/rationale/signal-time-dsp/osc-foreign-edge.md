# Why: OSC-the-binary-protocol lives only at the engine's foreign edge — external controllers in, osc_out nodes out — while every door ships the same flat {address, args} form in its own local framing and converges at the engine's one control ingress.

[Rule](../../signal-time-dsp.md#osc-foreign-edge)

The core speaks OSC-*shaped* Messages ([osc-only-core](osc-only-core.md)); that is a claim about the
data shape, not about a binary protocol on a socket. Modelling an internal door as *external* control
— encoding core values into OSC datagrams so our own peer can immediately decode them — is pure
ceremony, and it is not free: it makes each door own a socket and a wire format to talk to itself,
pull in a packet library for type tags, bundles, timetags, and 4-byte padding it would immediately
have to forbid, and probe liveness first because UDP is silent about a dead port.

So each door spells the same `{address, [args]}` pair in **its own local framing** — the browser's
hand-rolled flat codec, the loopback channel's JSON, `reuben play`'s OSC datagrams from the outside
world — and all of them converge at the engine's one control ingress, where the destination port's
declared type drives the conversion to the single typed value it carries. Because routing converges in
core, auditioning behaves identically whichever door it arrived through, and there is no second routing
path to keep in step: the loopback door's ingress is a clone of the very sender the UDP decode thread
holds.

The channel's atom vocabulary is **three primitives** — an integer, a number, a string — deliberately
not the central engine value type, which also carries notes, harmonies, pitches, erased enums, and a
whole sample buffer: a vocabulary a control wire would immediately have to forbid, and one whose
serialization has no business hanging off the type the render thread passes around. On a JSON channel
the atoms ride bare, so the channel stays debuggable by hand and the integer-vs-float split falls out
of JSON's own spelling rather than a rule each client reimplements.

The loopback authoring door is therefore a **second control ingress** alongside external OSC. What it
buys beyond the deleted plumbing: a dead engine surfaces as a refused connect rather than a datagram
vanishing into a silent port. What it does not change: concurrent senders remain last-write-wins per
control, exactly as two physical controllers behave.

Distilled from: ADR-0065. The three-primitives and converge-at-one-ingress reasoning was harvested from
the window's `engine::wire` control types, where it also lives as mechanics.
