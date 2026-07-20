# Why: differentiate and integrate are dense Float-to-Float ops with a constant one-sample dt, which is what keeps higher-order calculus valid.

[Rule](../../signal-time-dsp.md#calculus-constant-dt)

`differentiate` and `integrate` are dense `Float`→`Float` ops with `dt` equal to **one audio sample,
unscaled**: `differentiate` is `out[i] = buf[i] − buf[i-1]`, `integrate` is a running Riemann sum
`acc += buf[i]`. The load-bearing property is that `dt` is **constant**. Higher-order calculus is only
valid on a constant sampling window — differentiate a signal twice and you get acceleration *only if*
`dt` does not vary — and an irregular sparse Δt cannot guarantee that. That is precisely why these ops
moved from the Message domain (where they computed Δt between sparse distinct events, i.e. gesture
velocity) to the dense Signal domain: the sample grid is the one window that never varies.

Two engine details preserve correctness across the block boundary: `differentiate` carries one sample
of state between blocks and seeds `last = buf[0]` on the very first sample so there is no startup
spike; `integrate`'s accumulator likewise persists across blocks. Both reset on `spawn`. Conversion to
a real time base ("change per second", "per beat") is deliberately a **separate, deferred** op, not
baked in — `dt` here is literally one sample. The old sparse-velocity behavior is recovered by
materializing the gesture into dense CV first (`m2s`/slew) and then differentiating it, so nothing is
lost by the domain move.

Distilled from: ADR-0029
