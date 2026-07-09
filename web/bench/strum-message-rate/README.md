# strum-harp control-message rate — measurement (P7/A1)

Resolves [#252](https://github.com/Impractical-Instruments/reuben/issues/252). Gates the
SharedArrayBuffer control ring, [#257](https://github.com/Impractical-Instruments/reuben/issues/257).

## Question

Under a hard continuous drag on `strum-harp`'s strum bar (`/strum/position`, the stress
case in [#229](https://github.com/Impractical-Instruments/reuben/issues/229)), does the
current `postMessage` control channel churn enough to justify a SharedArrayBuffer SPSC
ring — which would cost a COOP/COEP cross-origin-isolation deploy change?

## Method

The auto-UI fader (`crates/reuben-web/js/surface/render.mjs`, `buildFader`) sends **exactly
one control message per DOM `input` event, with no throttle**. So the control-message rate
_is_ the `input`-event rate of the real widget (`<input type=range min=0 max=1 step=0.001>`).
The `strum` op's per-string note plucks happen **inside the engine** (signal→event), not as
extra control messages, so they don't change the channel rate.

`measure.mjs` therefore measures the `input`-event rate directly — no WASM/AudioContext
boot needed — driving a continuous back-and-forth drag via pipelined CDP `mouseMoved`
events at a range of pointer cadences, and separately microbenches the **real**
`encodeControl` (`crates/reuben-web/js/codec.mjs`) plus a transferable `postMessage`
round-trip to a worker (mirroring `engine.send` → worklet).

```
CHROMIUM_PATH=/path/to/chrome node web/bench/strum-message-rate/measure.mjs
```

(Needs `npm i playwright`. Numbers below from headless Chromium 1194 at 60 Hz rAF; a
120 Hz display raises the realistic rows proportionally but not the conclusion.)

## Results

Control-message rate vs. pointer-move cadence (2 s continuous drag; representative run):

| Pointer cadence (device)            | msg/s sustained | msg/s peak |
|-------------------------------------|-----------------|------------|
| ~125 Hz — typical mouse             | ~70             | ~100       |
| ~120 Hz — hi-refresh phone/tablet   | ~65             | ~90        |
| ~500 Hz — fast gaming mouse         | ~265            | ~290       |
| ~1000 Hz — gaming mouse             | ~485            | ~580       |
| unbounded — synthetic ceiling       | ~1400           | ~1900      |

> Rows are labelled by **target** pointer cadence; the harness drove moves within ~1 % of each
> (it reports `actual_move_hz`). Sustained msg/s runs ~55–65 % of the move rate because
> consecutive moves that don't change the quantized `step=0.001` value (or coalesce within a
> frame) emit no `input` event — so the message rate scales with, but sits below, the raw move rate.

The `input` event is **not** frame-coalesced — its rate scales with the pointer-move rate
(roughly half to two-thirds of it; see the note above) — so the ceiling is set by
pointing-device sample rate, not display refresh. Real target devices
("whatever device you already have" — phones, tablets, laptops) sit at **~60–100 msg/s**.
Only exotic high-poll gaming mice reach the hundreds; none of those are the target.

Per-message cost:

- **Main thread:** ~3–7 µs encode + ~8–16 µs transferable `postMessage` ≈ **~10–23 µs/msg**.
  At 100 msg/s that is ~1–2 ms/s (~0.1–0.2 % of one core); at the 580/s gaming-mouse peak,
  ~13 ms/s.
- **Audio thread (the debt at `crates/reuben-core/src/engine.rs:17-19`):** each message
  costs one `Message.address: String` alloc; each render block that carries ≥1 message
  costs one `pending` `Vec` alloc/free (`std::mem::take` at `engine.rs:255` leaves an
  empty `Vec`, so the next push re-allocates). At 60–100 msg/s that is **~200–300 tiny,
  same-size allocations/s** on the audio thread — served from a warm allocator free-list,
  ≪ the 2.67 ms/quantum (128 @ 48 kHz) RT budget. Even the 1000 Hz-mouse extreme
  (~500 msg/s + ~one Vec/block) is ~1000 small allocs/s.

## Conclusion — NO-GO on the SAB ring (on current evidence)

At realistic target-device pointer cadences the worst continuous strum emits only
~60–100 control msg/s. The `postMessage` channel carries that easily. The flagged `pending`-Vec
churn is a real RT-safety debt whose removal is still owed — `process` must never allocate
(AGENTS.md) — but at these rates it stays well under the audio-render budget, so paying it down
is **not urgent**, and it does **not** justify the SAB ring's cost (COOP/COEP cross-origin
isolation, a real hosting/deploy constraint). Per the hard gate agreed while charting the P7
map, the ring is **ruled out of scope** and #257 is closed; the debt itself remains tracked at
`engine.rs:17-19`.

**Revisit only if** a future, denser gesture source changes the message rate — e.g. a
multi-touch surface streaming several positional axes at once, or a control bound to raw
`pointerrawupdate` (up to ~1000 Hz) rather than the range `input` event. Re-run this
harness; if sustained rate climbs past a few hundred msg/s _on a real target device_, the
ring is back on the table.

## Not directly measured

The NO-GO call rests on the **measured** message rate plus **analysis** of the handoff code —
two of #252's three asks. The heap churn is derived analytically from reading the Rust (not
benchmarked), and observable symptoms (artifacts, jank, underruns) are **not** observed here:
booting the full WASM AudioWorklet headless and detecting underruns reliably is out of
proportion to a no-go. Symptom observation is folded into the real-device confirmation
([#262](https://github.com/Impractical-Instruments/reuben/issues/262)), keeping the evidence
trail explicit. If the ring is ever reconsidered, an end-to-end xrun measurement under a
sustained drag would be the confirming step.
