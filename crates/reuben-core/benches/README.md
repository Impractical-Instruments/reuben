# Benchmarks

Two layers measuring the same `render_block` workload (ADR-0019). Both drive the fixed,
deterministic schedule in [`common/mod.rs`](common/mod.rs): 1 s of audio (375 × 128 @ 48 kHz),
a four-note chord on at frame 0, off at 0.5 s, across five instruments — `reverb`, `echo`,
`auto-filter`, `sampler-arp`, `autotune`. The instrument documents are bench-owned frozen
fixtures under [`fixtures/`](fixtures/), not library instruments.

## Local — wall-clock (criterion)

Realtime factor and ns-level timing for day-to-day work. Throughput is reported in
`elem/s`; divide by 48 000 for ×realtime.

```sh
cargo bench -p reuben-core --bench macro_criterion

# Before/after a change:
git switch main && cargo bench -p reuben-core --bench macro_criterion -- --save-baseline main
git switch -    && cargo bench -p reuben-core --bench macro_criterion -- --baseline main
```

## CI gate — instruction counts (iai-callgrind)

Deterministic; this is what gates PRs. Requires `valgrind` and the matching runner:

```sh
sudo apt-get install valgrind
cargo install iai-callgrind-runner --version 0.16.1   # must match the dev-dependency
cargo bench -p reuben-core --bench macro_iai
```

In CI the [`bench` job](../../../.github/workflows/ci.yml) compares the PR against its base
ref via [`perf-gate.sh`](../../../.github/scripts/perf-gate.sh): **warn > 3%, fail > 10%** on
`Ir` (instructions). See ADR-0019 for the why.

## Trend history

Every push to `main` also records each case's absolute `Ir` to the `bench-history` orphan branch
(ADR-0019, layer 1). Read the whole cross-commit series with:

```sh
git show bench-history:bench-history.jsonl
```

Each line is `{sha, commit_sha, date, run_id, layer, case, ir}` — e.g. filter the macro `reverb`
trend with `git show bench-history:bench-history.jsonl | jq -c 'select(.layer=="macro" and .case=="reverb")'`.
