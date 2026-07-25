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

The hardest of those obligations is that **two devices are two clocks**. The engine renders at the
output device's rate, which is the clock anchor. An input device runs its own callback on its own
crystal, and even at the same nominal rate the two drift, so any fixed resample ratio eventually
starves or floods the buffer between them. They meet at a lock-free SPSC ring: the input callback
maps device frames onto the instrument's *logical* input channels and commits whole frames, and the
output callback drains them, resampling to the engine rate at a ratio a servo steers continuously.
Both sides stay RT-safe after startup — the ring is preallocated, the resampler's state is two
frames, and every policy decision is arithmetic on values already in cache.

Reconciling clocks costs latency, and the shell **budgets** it rather than discovering it: a ring
floor deep enough to ride out the input device's delivery granularity, the resampler's one-frame
lookahead, and one core block of staging that makes the engine's pull causal. That budget is a
different quantity from the ring's *capacity*, which is much larger and is headroom rather than
delay — conflating the two is the standard way a live-input path acquires latency nobody chose.

What ties the edges together is that every one of them **degrades in the open**. A missed render
deadline, an empty ring, a full ring, an input channel the device cannot supply, a swap to an
input-binding engine with no input stream: each has one fixed answer — defined silence — and each is
counted through a single diagnostics surface a non-RT thread can snapshot. None of it is
configurable and none of it is silent, because the alternative is a shell that improvises under
pressure and a musician who has to diagnose it by ear.

## Rules

<a id="degradation-is-fixed-and-counted"></a>
### Every failure at a shell edge degrades to defined silence, is counted on the one diagnostics surface, and is never configurable — the shell knows and says rather than improvising.

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

- **Host shell** — the removable per-platform layer wrapping the embed surface: it owns devices, foreign protocols, and the callback that hosts Render, and owes the engine blocks on time and an honest account when it cannot deliver them.
- **Dark degrade** — a shell edge's fixed response to a reality mismatch: play defined silence, count it, warn once, never fail and never improvise.
- **Drift servo** — the control loop steering the input resample ratio to hold the ring's post-drain residual at a fixed floor, so the loop is independent of the host's variable callback size.
