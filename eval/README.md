# `eval/` — what authoring an instrument costs a model

The perf gate watches what the engine costs the CPU. This watches what the **agent surface** costs a
model: grounding tokens, repair rounds, and freehand JSON. Built for the
[#574 map](https://github.com/Impractical-Instruments/reuben/issues/574), whose destination is
authoring that a small, local model can drive cheaply through every door.

Every prototype on that map claims a win against one of three numbers, and none can be *judged*
until they exist:

| | metric | what moves it |
|---|---|---|
| **(a)** | **grounding tokens** | everything the sidecar hands back — server `instructions`, tool schemas, resources read, every tool result |
| **(b)** | **repair rounds** | `validate` calls that came back `ok: false` |
| **(c)** | **document characters** | instrument-document payload the model had to emit, **echoes included** |

## Two tiers over one door

Both drive the real `reuben-mcp` sidecar over stdio, so the measured surface is the actual door —
real schemas, real `instructions`, real report shapes. Nothing can drift from what a user's client
sees, and the grounding budget is counted for free because it arrives over the wire.

**The gate tier runs no inference.** Each task carries a hand-written *reference solution* — the
ideal call sequence a perfect model would make — replayed against the sidecar. What it reports is
the surface's **cost floor**, which is exactly what a new verb moves: `nudge("warmer")` collapses
the floor for the nudge task whether or not any model is smart enough to use it. So a prototype's
claim is checkable **before a single token of inference is bought**. This is the tier that gates CI.

```sh
cargo build -p reuben-mcp
cd eval && python3 -m reuben_eval.gate
```

**The live tier is a ladder anchored to hardware bands** — 8 / 16 / 32 GB of unified memory. The
question is not "does it pass" but **where the pass line sits**; a prototype earns its place by
moving that line down a band. Run on demand, never in CI.

```sh
OLLAMA_CONTEXT_LENGTH=32768 ollama serve      # NOT optional — see below
cd eval && python3 -m reuben_eval.live --rung 16gb
```

## The four tasks

Frozen by [#592](https://github.com/Impractical-Instruments/reuben/issues/592) and bound to
committed `instruments/` fixtures, so the workload moves with the engine:

| task | shape | fixture |
|---|---|---|
| `from_scratch` | build a tone from nothing | — |
| `tweak` | set one value | `voices/default-voice.json` |
| `nudge` | apply an intent word ("warmer") | `voices/default-voice.json` |
| `repair` | fix a document that won't load | `voices/default-voice.json`, one edge dangled |

**Pass is `validate` clean AND a structural assertion.** `validate` owns legality — the harness never
re-implements it — and the assertion owns "did the asked-for thing actually happen". Both are
needed: `scaffold_instrument` already emits a valid document, so *change nothing* would otherwise
score as success. `tests/test_tasks.py` is the forcing function; every test in it is a negative,
proving the assertions reject the degenerate passes.

The assertions are strict about **collateral damage**: a one-value tweak that also drops the
document's `doc` prose fails. That damage is what whole-document re-emission causes, and a metric
blind to it would miss the thing the map is chasing.

## Why the harness ships file tools

`swap` is path-only and the roster has no document-read tool, so a real authoring client necessarily
brings its own filesystem access — that is the file-sightedness `#no-resource-bytes` mandates and
[#583](https://github.com/Impractical-Instruments/reuben/issues/583) proposes to reverse. `read_file`
/ `write_file` / `read_guide` stand in for the host's own tools. This is modelling the client, not
inventing a fourth door: the reuben surface under measurement is still exactly the sidecar's roster.

## The tokenizer is pinned, and that is the point

`reuben_eval/tokenizer/` vendors `cl100k_base` plus frozen `\p{L}`/`\p{N}` codepoint tables, both
hash-checked before a single token is counted.

- **Stdlib only.** `.github/scripts/` is deliberately dependency-free; `tiktoken` is a Rust extension
  with two transitive deps that downloads its vocabulary over the network at first use.
- **Unrelated to any rung.** No pinned model uses cl100k, so re-pinning a rung can never re-baseline
  the gated trend. The number is a **size proxy** meant to compare across years, not a billing figure.
- **Frozen tables.** Building the character classes from live `unicodedata` would tie the trend to
  whatever Unicode version the runner's CPython bundles, so a routine Python upgrade would silently
  re-tokenize everything.

Correctness is held by `tests/test_tokenizer.py`, which differentially tests the encoder against real
`tiktoken` over this repo's own corpus plus a seeded Unicode fuzz. It needs the `dev` extra and skips
when absent, so it never runs in the gate.

Regenerating either artifact is a deliberate, reviewable baseline reset:

```sh
python3 eval/tools/gen_unicode_classes.py    # then refresh pins.json, and say so in the PR
```

## Traps worth knowing

- **Ollama defaults to a 4k context below 24 GiB of VRAM** — every rung on this ladder. Left at the
  default, the low rungs truncate the grounding budget and fail for a reason that has nothing to do
  with the model's ability. `OLLAMA_CONTEXT_LENGTH` must be set **on the server**; the
  OpenAI-compatible endpoint has nowhere to carry it per request. The harness records what it was
  told and refuses to guess.
- **"Temperature 0" is not the whole sampling story.** The Ollama Go runner silently discards penalty
  parameters, so the live tier records what the server reported rather than what it asked for.
- **The 8GB rung is the weakest pin on the ladder.** Nothing in the research shows a 3B sustaining a
  12-round MCP loop against real schemas. A universal failure there means *suspect the pin* before
  *the surface is too expensive*.

## Where things live

```
reuben_eval/mcp.py         MCP stdio client + the token ledger
reuben_eval/workspace.py   host file tools + the document-payload ledger (metric c)
reuben_eval/tasks.py       the four tasks, reference solutions, structural assertions
reuben_eval/runner.py      one task run, scored
reuben_eval/gate.py        deterministic tier + the CI gate
reuben_eval/live.py        live tier + the pinned rungs
tools/                     the frozen-table regenerator
```

CI wiring: `.github/scripts/eval-gate.sh` (the gate, mirroring `perf-gate.sh`) and the
`eval-gate` job in `.github/workflows/ci.yml`. On pushes to `main`/`dev` the numbers land as
`eval-history.jsonl` beside `bench-history.jsonl` on the trend branch, and the dashboard grows an
eval section — one place to look, with `ir` never holding a token count.

Model and tokenizer pins, with the evidence behind them:
[`docs/research/harness-rungs.md`](../docs/research/harness-rungs.md).
