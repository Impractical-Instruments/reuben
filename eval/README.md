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
| **(b)** | **repair rounds** | `validate_instrument` calls that came back `ok: false` |
| **(c)** | **document characters** | instrument-document payload the model had to emit, **echoes included** |

## Two tiers over one door

Both drive the real `reuben-mcp` sidecar over stdio, so the measured surface is the actual door —
real schemas, real `instructions`, real report shapes. Nothing can drift from what a user's client
sees, and the grounding budget is counted for free because it arrives over the wire.

**The gate tier runs no inference.** Each task carries a hand-written *reference solution* — the
ideal call sequence a perfect model would make — replayed against the sidecar. What it reports is
the surface's **cost floor**, which is exactly what a new verb moves: one intent word in collapses
the floor for the intent-word tasks whether or not any model is smart enough to use it. So a
prototype's claim is checkable **before a single token of inference is bought**. This is the tier
that gates CI.

```sh
cargo build -p reuben-mcp
cd eval && python3 -m reuben_eval.gate
```

The gate is **visibility, not a verdict**. A grounding metric that regresses lands as a warning
annotation and a point on the trend — never a build failure. A gate that FAILs on roster growth
would encode "the library must not grow", a non-goal
([#612](https://github.com/Impractical-Instruments/reuben/issues/612)); only two things fail it — a
reference solution that stops passing (a real engine break) or a metric the harness can't compute. A
derived **per-tool schema density** (schema bytes ÷ tool count) sits beside the totals so capability
growth (more tools, flat density) reads differently from bloat (denser schemas, no new tool) — the
one genuinely invisible regression.

**The live tier is a ladder anchored to hardware bands** — 8 / 16 / 32 GB of unified memory. The
question is not "does it pass" but **where the pass line sits**; a prototype earns its place by
moving that line down a band. Run on demand, never in CI.

```sh
OLLAMA_CONTEXT_LENGTH=32768 ollama serve      # NOT optional — see below
cd eval && python3 -m reuben_eval.live --rung 16gb
```

## The tasks

The first four shapes are frozen by
[#592](https://github.com/Impractical-Instruments/reuben/issues/592); all are bound to committed
`instruments/` fixtures, so the workload moves with the engine:

| task | shape | fixture |
|---|---|---|
| `from_scratch` | build a tone from nothing | — |
| `tweak` | set one value | `voices/default-voice.json` |
| `intent_word` | apply an intent word ("warmer") | `voices/default-voice.json` |
| `intent_fan_out` | apply an intent word that reaches nine targets ("looser") | `acid-techno.json` + the voices it nests |
| `repair` | fix a document that won't load | `voices/default-voice.json`, one edge dangled |

`intent_fan_out` is a **new series**, added with the intent verb: `intent_word` exercises one target
(`warmer` matches one of its three moves on that fixture), which is exactly the case where a fan-out
claim is invisible. Adding a task rather than re-pointing one keeps the frozen four comparable
across the whole trend.

**Pass is `validate_instrument` clean AND a structural assertion.** It owns legality — the harness never
re-implements it — and the assertion owns "did the asked-for thing actually happen". Both are
needed: `new_instrument` already lands a valid document, so *change nothing* would otherwise
score as success. `tests/test_tasks.py` is the forcing function; every test in it is a negative,
proving the assertions reject the degenerate passes.

The assertions are strict about **collateral damage**: a one-value tweak that also drops the
document's `doc` prose fails. That damage is what whole-document re-emission causes, and a metric
blind to it would miss the thing the map is chasing.

## Why the harness ships file tools

It used to be a necessity: `swap` was path-only and the roster had no document-read tool, so a real
authoring client necessarily brought its own filesystem access.
[#603](https://github.com/Impractical-Instruments/reuben/issues/603) and
[#604](https://github.com/Impractical-Instruments/reuben/issues/604) removed that necessity — the
roster now reads a document (`describe_instrument`) and writes one (the document verbs) without the
model ever seeing its bytes. `read_file` / `write_file` / `read_guide` stay for the opposite reason:
a real client *still has* them, so leaving them on the namespace is what lets the harness see a model
reach for them anyway. Their presence is a measurement now, not a crutch — and
[#624](https://github.com/Impractical-Instruments/reuben/issues/624) carries the open call about
whether the reference solutions should still use them. This is modelling the client, not inventing a
fourth door: the reuben surface under measurement is still exactly the sidecar's roster.

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
reuben_eval/tasks.py       the five tasks, reference solutions, structural assertions
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
