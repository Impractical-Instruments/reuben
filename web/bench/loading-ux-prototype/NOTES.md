# Sample-decode loading/progress UX — prototype notes (P7/B, #253)

**THROWAWAY.** `prototype.html` is a self-contained mock (inline CSS/JS, fake timing, no
engine). Open it directly in a browser. It exists so a human can *react to* candidate
loading UXs side by side and pick — it decides nothing.

Resolves toward: [#253](https://github.com/Impractical-Instruments/reuben/issues/253).
Map: [#251](https://github.com/Impractical-Instruments/reuben/issues/251).
AC anchor (#229): *sample rigs load with visible progress, never a frozen UI.*

## The question

For sample-carrying rigs, how should loading present to the player, and how does decode
stay off the critical path so the UI never freezes? (States, fetch strategy, decode
scheduling, failure handling.)

## What the prototype shows

Five presentations of the *same* simulated load, contrastable across rig size
(1 sample → 6 samples), connection (fast / slow / offline-stall), injected failure
(fetch 404 / decode error), and fetch strategy (eager / lazy):

- **A · Current binary skeleton** — what ships today (shimmer → ready; no phase, no
  progress; on a stall it shimmers forever — there is no fetch timeout).
- **B · Per-rig status line** — one line names the phase (Fetching → Decoding → Ready).
- **C · Aggregate determinate bar** — one bar over total bytes; colour flips
  fetch → decode → done.
- **D · Per-sample list** — one row per sample, each with its own phase + bar.
- **E · Lazy (load on first play)** — surface renders instantly; fetch+decode deferred to
  the first Play tap (audible latency on slow links).

## Ground truth from the code (see the report for detail)

- Decode is **Rust/WASM** (`crates/reuben-web/src/decode.rs`, hound), run synchronously
  inside `stage_resource` (`bridge.rs` → `shell.stage_sample_wav`). It is **not** in
  `process()` / `render()`.
- It runs on **two threads today**: the main-thread *discovery* instance decodes during
  the fetch-on-miss loop (result discarded) — this is the **frozen-UI** risk; and the
  *worklet* instance decodes again from the shipped raw WAV bytes during the `load`
  message handler — on the audio thread, but while `process()` is emitting silence.
- **No shipping Toy carries samples** (`web/toys.json`: groovebox, chord-player,
  strum-harp, euclidean-drums, mic-space — none reference `.wav`). The path is fully
  built + unit-tested but **latent**. Sample rigs exist in `instruments/`
  (`sampler`, `sampler-arp`, `granulator-demo`) but are not bundled.

## Verdict

TBD — a human-in-the-loop session runs the grilling and records the decision here + in the
#253 spec. Do not delete this prototype until that decision is captured.
