---
name: rust-hot-path-review
description: Review a reuben Rust diff for the one tension that matters — performance/RT-safety vs idiomatic Rust. Classifies each hunk hot (audio render thread) or cold, flags RT-safety defects on hot code and misplaced optimization on cold code, and stays silent on everything clippy already covers. Use when the user says "review the hot path", "RT-safety review", "perf vs idiom review", "is this allocation-free", "review for the audio thread", or asks whether a change is safe to run on the render thread.
---

# rust-hot-path-review

**This skill speaks only when performance and idiom are in tension** — pushing toward RT/perf on the
hot path, toward idiom on the cold path. **Silence everywhere else.** It is *not* a "good Rust"
linter: naming, error handling, API shape, and every other mechanical idiom are **deferred to clippy
(`-D warnings`) and the global `/code-review` skill**. It is the review mirror of the
[`create-operator`](../create-operator/SKILL.md) authoring skill, and references the canonical RT
rules at [operator-dev.md#rt-safe-render](../../../docs/agents/operator-dev.md#rt-safe-render) — read
that anchor for the *why*; this skill is the *how-to-spot-it*.

## Run it

Default target is the working diff (`git diff` + staged); a branch or PR range works too. Execute the
three steps in order and emit the readout. **Don't restate clippy.** If you're unsure a hunk is on
the render thread, trace it — the classification is the whole job.

## Step 1 — Classify every changed hunk (the load-bearing core)

One question per hunk: **does this code run on the audio render thread(s)?** Bucket by *which thread
runs it*, never by file or type — **the boundary cuts through a single file** (`spawn` allocates by
design inches from an alloc-free `process`).

- **HOT** = reachable from a `fn process` body, *and* the per-block render path
  (`render_block` / `render_into` / `process_node` in `crates/reuben-core/src/render.rs`), *and* the
  message drain/route that runs on the audio thread.
- **COLD** = everything else: `descriptor()` / `operator_contract!`, `new` / `Default` / `spawn` /
  `bind_resources`, `RenderContext::new` preallocation, the whole **Coordinator** region (Instantiate,
  Swap-construction, (de)serialization, reclaim), and patcher / schema / registry / CLI.

Show the buckets before any finding, so the lens is auditable.

## Step 2 — On HOT hunks: flag RT defects, defend RT-correct un-idiom

These are **defects, not opinions** — they glitch or crash the audio thread. Each → **Blocking**.

- **Hidden allocation** → `hoist-or-preallocate`. Tells: `.collect()`, `format!` / `String`,
  `vec![]` or `Vec::push` past capacity, `Box::new`, `.to_vec()` / `.clone()` on a heap type,
  trait-object or closure boxing.
- **Panic** → `make-total`. Tells: `unwrap` / `expect` / `panic!` / `unreachable!` / `assert!` /
  `todo!` / `unimplemented!`. Fix with the codebase's totality idioms:
  `.unwrap_or(..)`, `.map_or(..)`, `.clamp(0.0, 1.0)` (see `operators/envelope.rs`; typed
  handles make port reads total by construction — `io.read(IN_GATE)` returns a defaulted value,
  and a Signal read is always exactly `io.frames()` samples, ADR-0037). A panic in the cpal
  callback unwinds across the FFI boundary.
  - **`debug_assert!` is blessed** — it vanishes in release; prefer it for hot-path invariants.
  - **Plain in-bounds indexing is exempt** — `buf[i]` where `i < n` (the `for i in 0..n` pattern) is
    fine; do not flag it.
- **Lock / blocking** → `make-total` (move it off-thread). Tells: `Mutex::lock`, blocking `recv`,
  syscalls, file I/O, logging.
- **`unsafe`** → `benchmark-or-remove`. **Hard line.** Admissible *only* with a committed benchmark
  ([ADR-0019](../../../docs/adr/0019-performance-benchmarking.md)) proving it's a measured hot path and
  safe Rust was the bottleneck. The core has **zero `unsafe`** today; keep it that way unless a number
  says otherwise.
- **Determinism** (one-line scan, not this skill's chapter — see
  [the guide's invariants](../../../docs/agents/authoring.md#invariants-you-must-not-break)):
  `HashMap`/`HashSet` iteration order, `Instant::now`, unseeded `rand`, threads racing on
  render output.

**Defend, don't flag, RT-correct un-idiom.** `std::mem::take` buffer swaps, preallocated scratch
`Vec`s reused via `drain(..)`, `SmallVec<[_; 8]>` inline capacity (all in `render.rs`) are *correct* —
a naive reviewer calls them un-idiomatic; you don't. **Stay silent on indexing style and
iterator-vs-loop** — the optimizer handles it and the codebase mixes `for i in 0..n { buf[i] }` and
`for x in buf.iter_mut()` freely.

## Step 3 — On COLD hunks: one opinion only

Flag **misplaced optimization** → `simplify-to-idiomatic`: perf-motivated complexity, `unsafe`, or
hand-tuning that buys no *measured* performance where performance doesn't matter. Each → **Advisory**.
Everything else on cold code — naming, error handling, API shape — is **not yours**: defer to clippy +
`/code-review`.

## Readout

```
## Classification
HOT:  <files/hunks on the render thread>
COLD: <the rest>

## Blocking — RT defects (hot path)
- `file:line` · HOT · <one-line tension> · <verdict> · <fix idiom from reuben>

## Advisory — misplaced optimization (cold path)
- `file:line` · COLD · <one-line tension> · simplify-to-idiomatic · <idiomatic form>

## Deferred (not reviewed here)
Cold-path idiom — naming, error handling, API shape — deferred to clippy + /code-review.

## Summary
N blocking hot-path findings (toward RT/perf), M advisory cold-path findings (toward idiom).
```

**Verdict vocabulary** (use these exact words): `benchmark-or-remove` · `make-total` ·
`hoist-or-preallocate` · `simplify-to-idiomatic`.
