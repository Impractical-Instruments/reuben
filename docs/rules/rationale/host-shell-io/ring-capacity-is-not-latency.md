# Why: Added input latency is a stated budget in parts, and the ring's capacity is headroom that never becomes sustained latency.

[Rule](../../host-shell-io.md#ring-capacity-is-not-latency)

Live input latency is the number a musician feels directly, so it is stated in parts that can each
be defended, rather than emerging from whatever the buffers happened to be sized at:

- the **ring floor**, deep enough to ride out the input device's own delivery granularity;
- the **resampler's lookahead**, one input frame — a rounding error today, and the term that grows
  if the [resampler is upgraded](resampler-quality-is-replaceable.md);
- one **core block of staging**, which is what makes the engine's pull causal.

At the shipped defaults that totals about three core blocks. It is dominated by deliberate safety
margin, and that is the honest characterization: the floor is not tuned to the minimum that works
once, it is set to what survives a device delivering unevenly, because a floor that occasionally
starves converts a latency saving into a counted underrun and an audible dropout.

The distinction the rule exists to protect is **capacity versus latency**, because conflating them
is the standard way this path silently acquires delay nobody chose. The ring is allocated far larger
than the floor — half a second of headroom — so that a stalled output callback has somewhere to put
arriving audio instead of dropping it. If fill were simply allowed to sit wherever a stall left it,
that headroom would become permanent added latency: the ring would run deep forever, the servo would
happily hold it there, and the only symptom would be that input feels late.

Three mechanisms keep capacity from turning into delay. An uncounted bulk trim re-anchors fill at
(demand + floor) every time warmup completes — at startup and at every re-prime — so a dry spell
never leaves the ring deeper than it started. The servo then holds it at that anchor. And the
high-water trim bounds any excursion a stalled consumer leaves behind at floor plus a fixed slack,
counted as an overrun, rather than letting it persist. The excursion bound is deliberately much
larger than the floor: it is there to catch a pathological stall, not to fight the servo, and a
tight bound would trim constantly during normal jitter and count overruns that mean nothing.

Decided in: issue #639 — settled directly, no ADR.
