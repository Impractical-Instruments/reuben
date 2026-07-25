# Why: An input device is a foreign clock, met at a lock-free ring, with the resample ratio steered from the ring's post-drain residual.

[Rule](../../host-shell-io.md#two-devices-are-two-clocks)

The engine renders inside the output callback at the output device's rate — that device is the clock
anchor, and everything else in the system is timed against it. An input device is not on that clock.
It has its own crystal, its own callback thread, and its own idea of how long a second is. Even two
devices agreeing on 48 kHz nominally will drift by tens of parts per million, which is inaudible per
second and fatal per hour: any *fixed* resample ratio accumulates that error until the buffer between
them either starves or floods. This is the USB-mic argument, and it is not an edge case — it is the
normal condition of any machine with a microphone that is not the built-in one.

So the two clocks meet at a lock-free SPSC ring, and the resample ratio becomes a **controlled
variable** rather than a constant. The servo measures the ring and steers the ratio to hold it at a
target, which converts an unbounded accumulating error into a bounded steady-state offset.

The non-obvious part is *what* it measures. The natural instinct is the ring's fill level, but fill
sampled at an arbitrary moment is dominated by where you are in the host's callback cycle, and the
host varies callback size freely — cpal makes no promise that two consecutive callbacks ask for the
same number of frames. A servo on raw fill therefore chases the host's scheduling rather than the
clock difference it exists to correct. Measuring the **residual** — what remains after the output
callback has drained its demand — subtracts the callback size out of the loop entirely, leaving a
quantity that moves only when the two clocks actually disagree. That is the whole reason the loop is
stable across a host that changes its buffer size mid-stream.

The correction is clamped and smoothed for musical reasons rather than control-theoretic ones. The
clamp (±0.5%, about 8.6 cents) bounds how far the servo can pitch-shift live input while chasing a
mismatch it cannot win: within that range the shift is inaudible on a live source, and beyond it the
honest answer is a counted overrun rather than an audible detune. One-pole smoothing keeps
producer-chunk granularity from jittering the ratio sample-to-sample, which would be heard as
warble even while the average ratio was correct.

Decided in: issue #639 — settled directly, no ADR.
