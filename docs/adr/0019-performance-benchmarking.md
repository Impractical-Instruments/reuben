# Performance benchmarking: two layers, a deterministic CI gate, and compare-against-base

## Context

`render_block` ([`render.rs`](../../crates/reuben-core/src/render.rs)) is the hot loop —
every operator's `process` runs under it, once per block, for the life of a stream. We had
no way to measure its cost and nothing to stop a change from silently making it slower. The
goal grilled down to a narrow one: **catch *major* regressions** in the render path, cheaply,
without a flaky gate that cries wolf.

Three facts framed the tree:

- **CI already exists** ([`ci.yml`](../../.github/workflows/ci.yml)) — one `check` job
  (fmt + clippy + test) on `ubuntu-latest`, a *shared* runner. Wall-clock timing there
  jitters ±10–30%, so a naive `cargo bench` gate would false-fail constantly.
- **Instruction counts don't jitter.** callgrind counts instructions executed — independent
  of CPU speed and of what else the runner is doing. The same code yields the same count.
  They *do* move with the toolchain (rustc/std/dep versions), which CI floats via
  `dtolnay/rust-toolchain@stable`.
- **The instruments are the realistic workloads.** Twelve real graphs live in
  [`instruments/`](../../instruments/); a bench can `load` → `Plan::instantiate` →
  `render_block` with no synthetic graph to invent or maintain.

## Decision

### Two layers, each matched to its environment

- **Local — criterion (wall-clock).** Answers "is it fast / how many ×realtime". `--baseline`
  gives a dev a local before/after. Never runs in CI (the shared-runner noise above).
- **CI gate — iai-callgrind (instruction counts).** Answers "did this PR regress" with no
  flake. This is the only perf job that gates.

They measure different things on purpose; neither replaces the other.

### Macro first, micro deferred

The benched unit is end-to-end `render_block` of a real instrument — that's what a user feels,
and a graph's cost is more than the sum of its operators (routing/dispatch live in
`render.rs`). Per-operator micro benches are a *diagnostic* layer added later; they need access
to crate-private `process` impls, a privacy bridge we deliberately did not design here.

### Workload: one fixed, deterministic schedule

48 000 Hz, block 128, **375 blocks = exactly 1 s**. A four-note chord (MIDI 60/64/67/72)
note-on at frame 0, note-off at 0.5 s, rendered through the release tail — loading polyphony
and exercising gate-on, sustain, and release. **No clock reads, no RNG** → iai counts are
byte-stable. The same `BenchState` drives both layers, so they measure identical work.

### Four instruments, chosen to span the heavy families

`reverb` (comb/allpass banks), `echo` (delay feedback), `auto-filter` (lfo + m2s + math →
filter modulation stack), `sampler-arp` (sample + clock + sequencer, the non-oscillator path).
Each covers a family nothing else does; the trivial graphs (`metronome`, `good-button`) are
skipped as redundant.

### The gate compares against a base ref, not a committed baseline

A committed numbers file would churn on every rustc release (floating `stable`). Instead the
gate benches **two refs with the same runner toolchain and diffs them**, so toolchain drift
cancels. The baseline ref is **the PR's base branch** (`pull_request.base.sha`) on a PR, or
**the previous commit** (`event.before`, falling back to `HEAD~1`) on a direct branch push.

Because the bench harness may not exist on the baseline commit, the gate swaps only
`crates/reuben-core/src` to the baseline and keeps the PR's benches for both runs
([`perf-gate.sh`](../../.github/scripts/perf-gate.sh)).

### Two-tier threshold, deterministic verdict

**Warn > 3%, fail > 10%.** "Major" is the 10% line — it catches accidental O(n²), a clone or
alloc in the per-sample loop — while ignoring benign refactor wobble. The 3% warning surfaces
slow creep without blocking. The hard fail is enforced by callgrind itself
(`--callgrind-limits='ir=10%'`, non-zero exit = the authoritative verdict); the per-instrument
table and 3% warnings are parsed best-effort from `--save-summary=json` and degrade gracefully
if the schema shifts. Results render to the job's **step summary** (no PR-comment write scope;
CI stays `contents: read`).

## Consequences

- A PR that regresses `render_block` by > 10% on any of the four instruments reds the `bench`
  job. Making that job a *required* check is a branch-protection setting, applied once it has
  proven itself over a few real PRs.
- The `bench` job runs in parallel with `check`, installs valgrind + a version-pinned
  `iai-callgrind-runner`, and benches only `reuben-core` (no ALSA/native).
- `check`'s clippy now also lints the bench code (`--all-targets`).

### Persisted trend on an orphan branch

The gate compares HEAD to its parent and discards the numbers — they survive only in the job's step
summary, which ages out, so there was no way to see a trend across commits without log archaeology.
Layer 1 of the trend plan persists them: [`perf-gate.sh`](../../.github/scripts/perf-gate.sh)
harvests HEAD's absolute `Ir` per benched case (parsed from the human-readable iai output, which —
unlike `--save-summary` JSON — still carries a value for cases that breached the gate), and a
dedicated `bench-history` CI job appends it to the **`bench-history` orphan branch** as JSONL. The
whole series is then one command away:

```sh
git show bench-history:bench-history.jsonl
```

That job is the **only** `contents: write` grant in CI — the gate itself stays `contents: read`. The
branch is orphaned (not `main`) so `main`'s tree never churns and recording never re-triggers CI. It
runs on direct pushes to `main` only, even when the gate redded (the `Ir` is still valid history),
and no-ops when the harness didn't compile against its baseline — an honest gap, not a fabricated
point. Harvesting is best-effort and never affects the gate verdict.

Layer 2 — visualization — renders that JSONL into the branch itself: the same CI job runs
[`bench-dashboard.py`](../../.github/scripts/bench-dashboard.py) (stdlib-only python) and commits a
`README.md` plus light/dark SVG charts beside the data, so **browsing the `bench-history` branch on
GitHub is the dashboard** — macro trend per instrument, a per-node engine-overhead proxy (the
cheapest value-rate micro case, whose Ir is ~all `step_node` overhead), the heaviest micro cases,
and a full latest/Δ table. No Pages setup, no external service, works on a private repo. Rendering
is best-effort: a dashboard bug never loses the data point.

## Deferred

- **Micro per-operator benches** — ✅ landed in #30. The crate-private access bridge is a
  feature-gated [`bench_support`](../../crates/reuben-core/src/bench_support.rs) module (non-default
  `bench` feature, so it never leaks into the public API): its `OpHarness` reaches the `pub(crate)`
  `Io` builders and drives one operator's `process` directly. A single `WORKLOADS` table is the
  source of truth — `micro_criterion` iterates it, `micro_iai` lists it, and a forcing-function test
  (run in `check` under `--features bench`) reds CI if it drifts from the operator registry or the
  iai gate list. Both layers, both the macro and micro sets, now gate (perf-gate.sh runs each
  independently).
- **Tonal-context resolver coverage** — ✅ also #30: the `autotune` instrument (context → snap →
  voicer) joined the macro fixture set, exercising the `hz`/`snap`/`chord_tone` resolver and
  context-driven block-slicing (ADR-0013) that the original four fixtures never touched. The
  per-operator layer additionally micro-benches `snap`/`context`/`m2s` directly.
- **Promoting the 3% warn to a machine-enforced annotation** — currently best-effort from the
  summary JSON; harden once a real CI run confirms the 0.16 schema path.
- **Marking `bench` a required check** — after a bake-in period.
