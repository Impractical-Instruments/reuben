# Why: Input resampling starts at the quality that is trivially RT-safe and bit-exact at matched rates, behind a replaceable seam.

[Rule](../../host-shell-io.md#resampler-quality-is-replaceable)

Resampler quality is the classic place to over-invest early. A windowed-sinc converter is
unambiguously better on paper, and it also brings FIR history to preallocate, a library or a
hand-rolled kernel to audit for allocation, and a design that is harder to reason about on the audio
thread — paid up front, against a benefit almost nobody in the common case receives.

Linear interpolation over a two-frame window is chosen instead, for three reasons that are about the
*shape* of the problem rather than about interpolation quality:

- It is **trivially RT-safe**. Two frames of state, no history buffer, no allocation, no dependency.
  There is nothing to audit.
- It is **bit-exact in the dominant case**. Matched device and engine rates put the servo at a ratio
  of 1.0, where linear interpolation is straight passthrough. The overwhelmingly common
  configuration — one device, one rate — pays literally nothing and loses no bits, which also means
  the determinism story for matched rates needs no asterisk.
- At the ratios [drift compensation](two-devices-are-two-clocks.md) actually produces (|1 − r| ≤
  0.5%), its passband error sits far below the noise floor of any live microphone path. The servo's
  clamp and the resampler's weakness are matched to each other by construction.

The case it is genuinely weak on is a real rate mismatch — a 44.1 kHz microphone into a 48 kHz
engine — where linear interpolation images above roughly 17 kHz. That is audible on bright synthetic
material and acceptable for voice and instrument capture, and recording it as a known limitation is
the point: modest starting quality is a decision, not an oversight.

What makes the decision cheap to revisit is that the resampler is **one component behind a seam**,
not a design assumption threaded through the input path. The ring, the servo, the degrade policies,
and the latency budget all treat it as "something that consumes input frames and produces engine
frames at a ratio." Swapping in windowed-sinc changes the component and the lookahead term of the
[latency budget](ring-capacity-is-not-latency.md), and nothing else. Keeping that seam intact is the
obligation this rule actually imposes — the starting quality is allowed to be modest precisely
because the upgrade path stayed open.

Decided in: issue #639 — settled directly, no ADR.
