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

Rewriting a reference is itself a **re-baseline**, not a regression: every metric for that task moves
at once, which looks identical to the surface getting dearer or cheaper. So `tasks.py` carries a
`REFERENCE_REVISION`, the gate says so when two compared reports disagree on it, and the number rides
`eval-history.jsonl` so the dashboard names the commit where the trend steps.

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
document's `doc` prose fails. Two different mechanisms produce that damage — a whole-document
re-emit, and a second, unasked-for verb call — so the failure reports **what** moved
(``also changed /filter.doc``) and leaves the cause to the trace. A metric blind to either would miss
the thing the map is chasing.

## Why the harness ships no file tool

It shipped `read_file` / `write_file` for a good reason, and that reason was **a measurement, not a
crutch**: a real client still has `Read`/`Write`, so leaving them on the namespace was what let the
harness watch a model reach for them anyway. Their absence would have hidden a real behaviour rather
than fixing it.

That observation has been deliberately traded for a **stronger one**. The roster reads a document
(`describe_instrument`) and writes one (the document verbs) without the model ever seeing its bytes,
so a conforming client has no reason to touch instrument JSON at all. The tools come off, and on both
tiers **`file-access` — an agent reached outside the roster for the document — becomes a named
failure mode**: the run fails and the report says so on its own line, classified apart from a
malformed call so it is legible at a glance rather than looking like a typo. The message names the
call reached for and stops there; asserting a read or a write would over-claim, since a shell and an
interpreter are classified on the same evidence and neither operation is ever observed.

What that detects is the **reach**, not a completed operation — nothing in the harness can complete
one any more. That distinction is what keeps the check live rather than trivially green forever.
Removing the tools from reuben's roster does not remove them from the world: `source` is still a
filesystem path on the native and sidecar doors, and a real host — Claude Code, say — brings its own
Read and Write that reuben cannot take away, so the old path stays walkable outside the harness. The
new arrangement is therefore strictly better than "leave the tools on and watch", because it
separates what a model *wants* from what the surface happens to offer it.

The classification matches a name rather than enumerating two, since the whole point is to catch the
reach *after* `read_file` is gone and whatever a model's priors call the same move arrives instead.
It is a **union** of four cheap rules: a filesystem noun in any word (`readFile`, `move_file`,
`directory_tree`), a shell or editing word in any word (`execute_command`, `run_terminal_cmd`,
`apply_patch`), a whole-name match for the bare verbs (`Read`, `Write`, `Edit`, `cat`), and a
fragment match for the editor family whose spelling keeps moving (`str_replace_based_edit_tool`).

Every one of those rules exists because a narrower one missed something real. An AND of
action-and-noun would be *stricter* than the two literal names it replaced — `Read` and `Write` are
bare verbs with no noun. A whole-name shell set catches the category words and misses every product:
`execute_command` (Cline), `run_shell_command` (Gemini CLI), `run_terminal_cmd` (Cursor),
`execute_bash` (OpenHands), `local_shell` (OpenAI) are what agents actually ship. A miss there is a
model falling back to the old path and being reported as having stopped wanting it.

Being generous is safe because the matcher is only ever consulted on a name the roster already
refused — it cannot shadow a verb, and a test walks the live roster to prove none of them trips it. A
check that cannot be made to fail is not a check, so `tests/test_tasks.py` also drives a run that
emits `read_file` after an otherwise-perfect edit and asserts the run fails with the mode named.

Two things are deliberately outside this:

- **`read_guide` stays.** It reads grounding prose that is *meant* for the model's context, not a
  reuben-owned document. Reading a guide is not reading a file.
- **Seeding survives, harness-side.** The `repair` task needs its broken document on disk, and
  `Workspace` writes it at construction — the harness's own side of the wire. The separation is
  structural rather than conventional: nothing in the model-facing roster can read or write a file,
  and there is no host dispatch left for one to route to.

Metric (c) is priced in two halves, on the same principle. A **roster** call is charged **by name**:
it is bound by a schema, so what carries a document is a fact known in advance — and that table is
now empty, because no arm takes one by value any more. An **invented** call is charged **by value**:
read the argument, never its label. It qualifies if its serialisation runs past ~200 characters,
which is where an argument stops being communication whatever it is called, or if it parses as JSON
carrying an instrument's keys, which catches a near-empty document that slips under the size floor.

Reading the value rather than the name is what makes this hold. Any list of argument names is one
agent product behind the next one shipped: `patch` is OpenAI Codex's `apply_patch`, `file_text` is
Anthropic's text editor, and a model inventing a call is bound by neither. `Write(content=…)`,
`write_file(text=…)` and `apply_patch(patch=…)` are one emission in three spellings, and are priced
identically. A false zero is the one direction this metric must never be fooled in now that it is a
floor and a tripwire rather than a spread of values.

A verb argument is not a document even when it is structured: `add_instrument_node(inputs=…)` and
`send_live_controls(messages=…)` cost nothing. (c) prices **freehand JSON** — the document the model
had to compose and hold — not communication, and a node's inputs map is the thing the verbs exist to
make cheap. The name-keyed roster half is what structurally keeps the value rule from reaching one.

The practical consequence: a passing gate-tier run prices at **zero**, and a non-zero number means
bytes were emitted somewhere — usually a document, sometimes long prose at an invented call. The
200-character floor is a size heuristic, so it cannot tell the two apart, and it is not trying to:
the error runs only toward *more* expensive, which is the direction a cost metric may never be fooled
in. It is a tripwire, not a proof.

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
reuben_eval/workspace.py   the host roster, task seeding, the document-payload ledger (metric c)
reuben_eval/tasks.py       the five tasks, reference solutions, structural assertions
reuben_eval/runner.py      one task run, scored
reuben_eval/gate.py        deterministic tier + the CI gate
reuben_eval/live.py        live tier + the pinned rungs
tools/                     the frozen-table regenerator
```

CI wiring: `.github/scripts/eval-gate.sh` (the gate, mirroring `perf-gate.sh`) and the
`eval-gate` job in `.github/workflows/ci.yml`, whose `bench-history` job routes each long-lived
branch to its own trend. The numbers land as `eval-history.jsonl` beside `bench-history.jsonl`
there, and the dashboard grows an
eval section — one place to look, with `ir` never holding a token count.

Model and tokenizer pins, with the evidence behind them:
[`docs/research/harness-rungs.md`](../docs/research/harness-rungs.md).
