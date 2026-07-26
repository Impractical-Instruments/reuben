# Host shell & native I/O

> What a host shell owes the engine at the edges it owns — devices and their foreign clocks, the resampling and drift compensation that reconcile them, the latency that buys, and the fixed, counted way every edge degrades.

## Now

[Execution & runtime](execution-runtime.md) ends at the **embed surface**: the engine guarantees a
deterministic, allocation-free block given inputs on time. Everything on the other side of that rim
is a **host shell** — the native layer here, a browser worklet in the product repo — and a shell has
obligations of its own. It opens devices, negotiates a real sample rate and buffer size against what
hardware grants, converts foreign protocols to Messages at its edge, hosts Render inside a callback
it does not control the timing of, and reconciles clocks the engine is entitled to assume are one
clock. None of that is engine behavior, and all of it is load-bearing: a shell that gets it wrong
produces a system that is deterministic and correct and still sounds broken.

Three obligations carry the weight, and each has its own rule below: an input device is a second
clock the shell must servo against, the latency that costs is a stated budget rather than whatever
the buffers happened to be, and every edge degrades to defined silence in the open rather than
improvising under pressure.

## Rules

<a id="degradation-is-fixed-and-counted"></a>
### Every failure at a shell edge degrades to defined silence, is accounted for — counted if it can recur, warned once if it cannot — and is never configurable: the shell knows and says rather than improvising.

[why](rationale/host-shell-io/degradation-is-fixed-and-counted.md)

<a id="two-devices-are-two-clocks"></a>
### An input device is a foreign clock, so the shell meets it at a lock-free ring and steers the resample ratio from the ring's post-drain **residual** — never from a fixed ratio or the pre-drain fill.

[why](rationale/host-shell-io/two-devices-are-two-clocks.md)

<a id="resampler-quality-is-replaceable"></a>
### Input resampling starts at the quality that is trivially RT-safe and bit-exact at matched rates, behind a seam that a better resampler can replace without touching the ring, the servo, or the degrade policies.

[why](rationale/host-shell-io/resampler-quality-is-replaceable.md)

<a id="ring-capacity-is-not-latency"></a>
### Added input latency is a budget the shell states in parts — ring floor, resampler lookahead, staging block — and the ring's capacity is headroom that never becomes sustained latency.

[why](rationale/host-shell-io/ring-capacity-is-not-latency.md)

## Terms

- **Host shell** — the removable per-platform layer wrapping the embed surface; it owns devices, foreign protocols, and the callback that hosts Render.
- **Dark degrade** — a shell edge's fixed response to a reality mismatch: play defined silence, then count it if it can recur or warn once if it cannot; never fail and never improvise.
- **Drift servo** — the control loop steering the input resample ratio to hold the ring's post-drain residual at a fixed floor, so the loop is independent of the host's variable callback size.
