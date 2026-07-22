# Pinning the eval harness: hardware-band models + the gate tokenizer

**Date:** 2026-07-22 (all sources accessed this day) ·
**Ticket:** [#597](https://github.com/Impractical-Instruments/reuben/issues/597) (wayfinder:research),
pinning values into [#592](https://github.com/Impractical-Instruments/reuben/issues/592) on the
[#574](https://github.com/Impractical-Instruments/reuben/issues/574) map.

**Research question, two parts:**

1. Which local model fills each rung of the live tier — "fits in 8GB / 16GB / 32GB of unified memory
   at usable speed" — pinned by exact runner tag and quantization, with **native tool-calling over an
   OpenAI-compatible endpoint reliable enough to survive a multi-turn MCP loop**?
2. Which tokenizer gets vendored into `eval/` and pinned by hash as the deterministic tier's **size
   proxy** — judged on stability, licensing, and vendorability, not fidelity to any rung?

Method: primary sources — Ollama's registry manifest API (exact blob digests and byte sizes), Qwen
and IBM model cards on Hugging Face, the Hugging Face model API, the `ollama/ollama` and
`ggml-org/llama.cpp` issue trackers via `gh`, `openai/tiktoken` source, CPython's `re` docs — plus
**one experiment run locally today** (a stdlib-only cl100k_base encoder differential-tested against
`tiktoken` 0.13.0, §6.3). Where the evidence is thin it is labelled thin, because a wrong pin dates
the whole harness.

---

## 0. The one number that decides every band

The bands are *unified memory*, so the binding constraint is not RAM but **Metal's
`recommendedMaxWorkingSetSize`** — the ceiling Ollama and llama.cpp treat as hard when deciding
whether a model goes on the GPU. macOS derives it from total RAM: **~66% at or below 36GB, ~75%
above**, tunable only by `sudo sysctl iogpu.wired_limit_mb=<N>` and reset on reboot
([devnote-override-macos-metal-vram-cap](https://github.com/ivanopcode/devnote-override-macos-metal-vram-cap);
community-measured, not Apple-documented — flagged as such).

| Band | Total unified memory | Default Metal working set | Realistic weights budget (leaving KV + compute buffers) |
|---|---|---|---|
| **8GB** | 8 GB | ~5.3 GB | **≤ 4.0 GB** |
| **16GB** | 16 GB | ~10.6 GB | **≤ 8.0 GB** |
| **32GB** | 32 GB | ~21.1 GB | **≤ 18.0 GB** |

Everything below is measured against that table. A pin that only works after
`sudo sysctl` is not a pin — it is an asterisk on every number the ladder ever reports.

---

## 1. The candidate field as of today

`ollama.com/search?c=tools` (accessed 2026-07-22) lists the tool-capable library. The families that
matter for consumer unified memory, with byte-exact sizes from `registry.ollama.ai`:

| Family | License | Sizes on Ollama | Arch | Ollama pulls |
|---|---|---|---|---|
| **qwen3.5** | Apache-2.0 | 0.8B / 2B / 4B / 9B / 27B / 35B-A3B / 122B-A10B | hybrid Gated DeltaNet + sparse MoE | 16M |
| **qwen3.6** | (not stated on page) | 27B / 35B-A3B | Gated DeltaNet + Gated Attention | 4.4M |
| **gemma4** | Gemma Terms of Use | E2B / E4B / 12B / 26B-A4B / 31B | dense + MoE mix | 19.1M |
| **granite4.1** | Apache-2.0 | 3B / 8B / 30B | **dense decoder-only** | 270.1K |
| **gpt-oss** | Apache-2.0 | 20B / 120B | MoE (~3.6B active) | 11.1M |
| **nemotron3** | NVIDIA Open Model | 33B | — | 621.2K |

Immediate eliminations:

- **gpt-oss:20b** is a single 14 GB blob. It clears the 32GB band trivially and misses the 16GB
  band's ~10.6 GB ceiling — it cannot form a ladder, only occupy one rung.
- **gemma4** is not Apache-2.0 (Gemma Terms of Use carry a use policy); its sizes land awkwardly
  (E2B-it-qat 4.3 GB, 12b-it-q4_K_M 7.6 GB, 31b-it-qat 19 GB). Worth revisiting — Ollama v0.32.1
  (2026-07-16) shipped "Improved Gemma 4 tool calling and multi-turn reasoning, including more
  reliable tool-response continuations", which is the only *release-note-level* tool-calling
  investment any family got this quarter.
- **qwen3.6** has no published model-card benchmark set comparable to 3.5's and covers only 27B/35B —
  no 8GB or 16GB rung exists.

That leaves a genuine two-horse race: **Qwen3.5** (best benchmarks, best adoption) and
**Granite 4.1** (best fit, best plumbing).

---

## 2. Qwen3.5 — the strongest models, the weakest plumbing

### 2.1 What the model cards say

All Apache-2.0, all 262,144 native context (YaRN to ~1.01M), all released Feb 2026
([Qwen/Qwen3.5-4B](https://huggingface.co/Qwen/Qwen3.5-4B),
[-9B](https://huggingface.co/Qwen/Qwen3.5-9B),
[-27B](https://huggingface.co/Qwen/Qwen3.5-27B),
[-35B-A3B](https://huggingface.co/Qwen/Qwen3.5-35B-A3B)):

| Model | Arch | BFCL-V4 | TAU2-Bench | Ollama tag (Q4_K_M) | Weight bytes |
|---|---|---|---|---|---|
| Qwen3.5-4B | hybrid GDN + sparse MoE | 50.3 | 79.9 | `qwen3.5:4b-q4_K_M` | 3,389,971,840 (3.16 GiB) |
| Qwen3.5-9B | hybrid GDN + sparse MoE, 32L | 66.1 | 79.1 | `qwen3.5:9b-q4_K_M` | 6,594,462,816 (6.14 GiB) |
| Qwen3.5-27B | hybrid GDN + sparse MoE, 64L | 68.5 | 79.0 | `qwen3.5:27b-q4_K_M` | 17,420,420,832 (16.2 GiB) |
| Qwen3.5-35B-A3B | MoE, 256 experts, 8+1 active | 67.3 | 81.2 | `qwen3.5:35b-a3b-q4_K_M` | 23,869,179,840 (22.2 GiB) |

The BFCL-V4 numbers are corroborated independently by
[llm-stats.com/benchmarks/bfcl-v4](https://llm-stats.com/benchmarks/bfcl-v4) (updated 2026-07-22),
which has Qwen3.5 as **the only open-weight family present at all three sizes** —
0.503 / 0.661 / 0.685 against a 0.750 leader. That is a genuinely comparable capability ladder, and
no other open family offers one.

**Treat the TAU2-Bench column as unusable.** 4B scores 79.9 and 27B scores 79.0 — a 4B model does
not out-agent a 27B model on a multi-turn tool benchmark. These are self-reported, the harness
configuration is undocumented, and the inversion is a red flag. Use BFCL-V4 only.

### 2.2 The disqualifying problem: multi-turn tool plumbing is broken in both runners

Qwen3.5 was trained on the **Qwen3-Coder XML** tool format
(`<function=name><parameter=k>v</parameter></function>`), not Hermes JSON. Both mainstream local
runners currently mishandle it in exactly the multi-turn agentic loop this harness *is*.

**Ollama — [#14493](https://github.com/ollama/ollama/issues/14493), OPEN since 2026-02-27, still
open at v0.32.2 (2026-07-20).** Source-level report against `qwen3.5:27b-q4_K_M`:

- The registry config wires `renderer: "qwen3.5"` / `parser: "qwen3.5"` to the **Qwen3 Hermes-JSON**
  pipeline. The correct `Qwen3CoderRenderer`/`Qwen3CoderParser` exists in-tree but is wired only to
  `qwen3-coder`.
- When an assistant turn has thinking + tool calls and no text, the renderer never emits `</think>`,
  so the tool call is rendered **inside an unclosed `<think>` block, corrupting every subsequent
  turn**. The *parser* side was fixed in v0.17.3 ([`d98dda4`](https://github.com/ollama/ollama/commit/d98dda4676d44a3882fd38492cc00db257f35974)); the *renderer* side was not.
- Penalty sampling (`presence_penalty`, `repeat_penalty`, `frequency_penalty`) is **silently
  discarded** on the Go runner, which Qwen3.5 is forced onto. Ollama's own modelfile for
  `qwen3.5:9b` ships `PARAMETER presence_penalty 1.5` — a parameter the runner then ignores.
- The proposed fix, [PR #15224](https://github.com/ollama/ollama/pull/15224) (2026-04-02), is
  **still unmerged**.

The independent A/B in that thread (2026-06-12, `qwen3.5:35b-mlx`, M4 Max 128GB, Ollama v0.30.7,
~15.5K-token prompt with 35 tool schemas, 5 trials/case) is the clearest evidence available:

| Case | Stock `qwen3.5` renderer | Same weights, `RENDERER qwen3-coder` |
|---|---|---|
| "relay a message" (should call a tool) | **2/5** | **4/5** |
| corrective re-prompt ("call it NOW") | **1/5** | **4/5** |
| weather w/ tool available | fabricated a full forecast, no call | real tool call, zero fabricated successes |

A model that *claims success without calling the tool* would score the harness's structural
assertion as a hard fail while telling you nothing about the surface. That is measuring the runner.

[#14745](https://github.com/ollama/ollama/issues/14745) ("`qwen3.5:9b` sometimes prints out tool call
instead of executing it", closed 2026-03-27) is the same failure at the 16GB rung.

**llama.cpp — [#20260](https://github.com/ggml-org/llama.cpp/issues/20260), OPEN since 2026-03-09.**
`llama-server --jinja` uses the GGUF's own ground-truth template, so the render side is right — but
the post-generation `peg-native` parser has `root ::= tool-call`, so **any prose the model emits
between `</think>` and `<tool_call>` produces a 500 `Failed to parse input at pos N` and kills the
turn.** Thinking models emit exactly that transition sentence constantly. A later comment
(2026-06-19) pins a regression window for Qwen3.6 at builds b9654→b9659 with duplicated
`</parameter>` output. [PR #24329](https://github.com/ggml-org/llama.cpp/pull/24329)
(merged 2026-06-15) only made the failure *soft and visible*; it did not fix the prefix-text case.
A 2026-07-18 comment reports a clean pass on b10066 — but for **Qwen3-Coder-Next**, not Qwen3.5.

**Verdict:** Qwen3.5 is the best-scoring, most-run open family at every band, and its tool-calling
is broken in the exact axis this harness stresses, in both runners, today. The break is *plumbing*,
not the model — which means it will probably be fixed, and means a re-pin to Qwen3.5 should be
revisited once #14493 closes.

### 2.3 The MoE band question, answered

The ticket asks whether a 30B-A3B belongs in a lower band than its parameter count suggests. Both
readings converge on the same rule:

> **The band is decided by resident footprint. Active parameters decide only tokens/sec.**

`qwen3.5:35b-a3b-q4_K_M` is **23.87 GB of weights**. The 32GB band's default Metal working set is
**~21.1 GB**. So the MoE that "runs like a 3B" **does not fit the 32GB band at all** — it is a
48GB/64GB-class model that happens to be fast. Its own `int4` build (20 GB) squeaks under the cap
with ~1 GB left for KV, which is not a pin, it is a dare. Measured speeds confirm the speed half:
Qwen3.6-35B-A3B hits **105 tok/s** decode at Q8 on an M5 Max vs **32 tok/s** for the dense
Qwen3.6-27B ([stared/benching-local-llms-on-apple-silicon](https://github.com/stared/benching-local-llms-on-apple-silicon),
2026-06-14), and 32 tok/s on a 24GB M4 Pro ([llmcheck.net](https://llmcheck.net/benchmarks)).

So: a sparse MoE buys you a lower band *in latency* and a **higher** band *in memory*. For a ladder
whose rungs are memory, that is a net loss — and it is why the recommendation below is an all-dense
ladder, where the band boundary has no ambiguity to flag.

---

## 3. Granite 4.1 — the fit and the plumbing

[ibm-granite](https://huggingface.co/ibm-granite) / [ollama.com/library/granite4.1](https://ollama.com/library/granite4.1):
**dense decoder-only** at 3B / 8B / 30B, **Apache-2.0**, 128K context standard with long-context
extension to 512K, ~15T training tokens, first-party GGUFs (`ibm-granite/granite-4.1-{3b,8b,30b}-GGUF`,
last modified 2026-04-20/21). IBM markets tool use and structured JSON output as headline
capabilities.

Reported BFCL **v3** scores: 3B = 60.80, 8B = 68.27, 30B = 73.68, with the 8B beating IBM's previous
32B MoE (68.3 vs 64.7). **These are v3, not v4 — they are not comparable to §2.1's numbers**, and
this is the single biggest evidence gap in this document.

### 3.1 Why the plumbing is better

Granite 4.1's Ollama manifest carries a **6,843-byte Go template layer**
(`sha256:89a0ab46e638…`), not a hardcoded Go renderer/parser pair. The template emits the
Hermes-style contract:

```
For each tool call, return a json object with function name and arguments within
<tool_call></tool_call> XML tags:
<tool_call>
{"name": <function-name>, "arguments": <args-json-object>}
</tool_call>
```

Three things follow:

1. **It is JSON inside a tag** — the oldest, most widely implemented tool format in both Ollama and
   llama.cpp, the one every parser was written against first.
2. **The template is data in the manifest, addressable by hash.** A re-pin cannot silently change
   the prompt shape; a diff of the layer digest catches it. Qwen3.5's renderer is compiled Go that
   changes with the Ollama binary.
3. The template author explicitly accommodated Ollama's tool-parsing heuristic (there is a
   `{{- if false }}` block whose only purpose is to expose `<tool_call>` to
   `ollama/tools/template.go`). That is deliberate integration, not an accident of naming.

**Tracker search returns zero open tool-calling issues for `granite4` in `ollama/ollama` and zero in
`ggml-org/llama.cpp`.** Be honest about what that is worth: Granite has 270.1K Ollama pulls against
qwen3.5's 16M, so absence of bug reports is partly absence of users. It is real evidence, but weak
evidence.

### 3.2 The fit is exact

| Band | Tag | Weight bytes | vs. band budget (§0) |
|---|---|---|---|
| 8GB | `granite4.1:3b-q4_K_M` | 2,099,501,664 (1.96 GiB) | ≤4.0 GB — **2.0 GB of headroom** |
| 16GB | `granite4.1:8b-q4_K_M` | 5,347,914,400 (4.98 GiB) | ≤8.0 GB — **3.0 GB of headroom** |
| 32GB | `granite4.1:30b-q4_K_M` | 17,490,240,736 (16.3 GiB) | ≤18.0 GB — **1.7 GB of headroom** |

All three land inside the default Metal working set with room for a real KV cache. No `sysctl`, no
asterisk. And all three are dense, so no MoE ambiguity exists to flag at any rung.

---

## 4. Recommendation: the model pins

### 4.1 Pin Granite 4.1, all three rungs, one family

| Rung | Model | Runner tag | Quant | Ollama manifest sha256 | Model blob sha256 | Weight footprint |
|---|---|---|---|---|---|---|
| **8GB** | IBM Granite 4.1 3B | `granite4.1:3b-q4_K_M` | Q4_K_M | `6fd349357287c7ffc9e38189a93b48ea175d24fc566b38f09cfc564fb7f303eb` | `662b0626cd58f443baea23559b469df6576a81d349649c59413b36a9fb32eb29` | 2,099,501,664 B |
| **16GB** | IBM Granite 4.1 8B | `granite4.1:8b-q4_K_M` | Q4_K_M | `444af1c4b2fedd6b54041aca558e7300b0b3d5c0468c44619126240323ba2852` | `ed902ac9eb6adce5a90c6a08c8ea201b50e23fdc5976d1cd0362006afac5309e` | 5,347,914,400 B |
| **32GB** | IBM Granite 4.1 30B | `granite4.1:30b-q4_K_M` | Q4_K_M | `3f3e5df8a021439fd6f867a0e526bdc303cac79c811201cb6bac193298cb9fcd` | `b33e4376e3581d11236ea53ced6b38399f6e91c0a391488486dc0827972f23f6` | 17,490,240,736 B |

Pin the **manifest sha256**, not just the tag: Ollama tags are mutable, digests are not. Equivalent
llama.cpp pins, if the harness prefers `llama-server --jinja` (which uses the GGUF's own template
rather than Ollama's Go path): `-hf ibm-granite/granite-4.1-{3b,8b,30b}-GGUF:Q4_K_M`, plus the
llama.cpp build number (`bNNNNN`), which is a strictly better reproducibility anchor than an Ollama
version string.

**Why not Qwen3.5, when it scores better and everyone runs it:** the harness's job is to report
where the pass line sits *for the surface*. With `qwen3.5` on Ollama the pass line moves when
`#14493` moves, and the failure mode — announcing a tool call in prose, or claiming success without
calling — reads as a structural-assertion failure indistinguishable from "the model couldn't do it".
That is not a measurement, it is noise correlated with the runner's release cadence. Granite trades
~5 BFCL points for a plumbing path that has not been reported broken.

### 4.2 What is genuinely thin here

- **Tokens/sec for Granite 4.1 on consumer Apple Silicon is not directly measured in any source
  found.** These are dense transformers, so they should track the well-measured dense curve: the
  9B-class rung ≈ 35 tok/s on an M1/16GB and ≈ 58 tok/s on an M3/16GB (llmcheck.net figures for
  Qwen3.5-9B Q4_K_M); the 30B rung ≈ 17–32 tok/s (Qwen3.6-27B dense, M5 Max Q8, with/without MTP).
  A 12-round loop at the 32GB rung will be minutes, not seconds. **Measure this on first run and
  record it — do not carry my extrapolation into the harness.**
- **Granite 4.1 has no BFCL-V4 entry.** Its v3 scores cannot be compared to Qwen3.5's v4 scores, so
  the claim "Granite 4.1 8B ≈ Qwen3.5 9B at tool calling" is an inference, not a measurement.
- **The 8GB rung is the weakest pin on the ladder.** Granite 4.1 3B at BFCL v3 60.80 is a plausible
  tool caller but nothing in the sources shows a 3B sustaining a 12-round MCP loop against real
  schemas. It may fail every task on day one — which is a legitimate ladder result, not a bug, but
  it should be smoke-tested before anyone concludes the *surface* is what failed.
- **Release date for Granite 4.1 is inconsistent across secondary sources** (2026-04-29 vs
  "early June 2026"). The HF GGUF `lastModified` of 2026-04-20/21 is the only primary anchor found.

### 4.3 The re-pin trigger to write down

Re-pin the ladder to Qwen3.5 (`4b` / `9b` / `27b` at `q4_K_M`, digests in §2.1) **when
`ollama/ollama#14493` closes and a smoke run reproduces the A/B's "corrective re-prompt" case at
≥4/5**. Qwen3.5 is the better ladder on every axis except the one that currently disqualifies it,
and it is what an actual user will be running. Until then it is the shadow, not the rung.

---

## 5. The tokenizer: candidates

Criteria from #592/#597: **stability across years**, **permissive license**, **vendorable as a
pinned artifact**, **unrelated to any rung**, and cheap to run in a CI gate job where
`.github/scripts/` is deliberately **stdlib-only** (see `bench-dashboard.py`: *"stdlib only: the
runner's system python3 is the whole toolchain"*).

| Candidate | License | Vendored artifact | Pure-Python cost | Notes |
|---|---|---|---|---|
| **cl100k_base** | tiktoken repo MIT; data file unlicensed (see §7) | `cl100k_base.tiktoken`, **1,681,126 B**, sha256 `223921b7…b2a7` | **Feasible, verified (§6.3)** | Pre-tokenizer uses only `\p{L}`, `\p{N}`, `(?i:)`, possessive quantifiers |
| o200k_base | same | 3,613,922 B, sha256 `446a9538…1a2d` | Harder | Pattern needs `\p{Lu} \p{Lt} \p{Lm} \p{Lo} \p{Ll} \p{M}` separately — 5× the hand-rolled surface |
| r50k_base / gpt2 | openai/gpt-2 MIT; `openai-community/gpt2` on HF is explicitly `license: mit` | 835,554 B (or vocab.json 1.04 MB + merges.txt 456 kB) | Easiest | **Over-tokenizes JSON and whitespace runs**, inflating the exact payload being measured |
| HF `tokenizer.json` + `tokenizers` | Apache-2.0 | any | N/A | Rust wheel dependency; also a per-vendor artifact |
| Qwen / Granite tokenizers | Apache-2.0 | any | N/A | **Disqualified by construction** — must be unrelated to any rung |
| SentencePiece (Llama/Gemma) | restrictive or protobuf-bound | `.model` | N/A | Dependency + license drag |

`tiktoken` itself as a pip dependency: **MIT**, latest **0.13.0** (2026-05-15), `requires_python
>=3.9`, `requires_dist: regex, requests` (+ optional `blobfile>=3`), binary wheels for
macOS/Linux/Windows × cpy3.9–3.13. Cheap to install — but it is a Rust extension plus two transitive
deps, and by default it *downloads* the vocab from `openaipublic.blob.core.windows.net` at first
use, putting a network fetch inside the gate. Both facts cut against the repo's stdlib-only CI rule.

---

## 6. Recommendation: pin `cl100k_base`, encode it in stdlib Python

### 6.1 The pin

```
eval/tokenizer/cl100k_base.tiktoken
  bytes:  1,681,126
  sha256: 223921b76ee99bde995b7ff738513eef100fb51d18c93597a113bcffe865b2a7
```

That hash is not mine — it is the `expected_hash` literal in
[`tiktoken_ext/openai_public.py`](https://github.com/openai/tiktoken/blob/main/tiktoken_ext/openai_public.py),
verified today by downloading the file and running `sha256sum`. Pinning the same value tiktoken
pins means any future drift is detectable against upstream, not just against ourselves.

### 6.2 Why cl100k and not the others

- **Stable.** Frozen since 2022 (GPT-3.5/4 era), hash asserted in tiktoken's own source across every
  release since. A size proxy that must compare across years wants a vocabulary nobody is still
  iterating on.
- **Unrelated to any rung.** No pinned model uses it. Qwen and Granite ship their own BPEs; using
  either would couple the deterministic tier to the live tier, which is precisely what #592 forbids.
- **Right shape for the payload.** cl100k has whitespace-run and code-punctuation tokens that r50k
  lacks; instrument documents are JSON, and r50k would exaggerate the very number the gate exists to
  watch.
- **Its pre-tokenizer is expressible in stdlib `re`.** This is the decisive practical point and it is
  where cl100k beats o200k. The cl100k pattern is

  ```
  '(?i:[sdmt]|ll|ve|re)|[^\r\n\p{L}\p{N}]?+\p{L}++|\p{N}{1,3}+| ?[^\s\p{L}\p{N}]++[\r\n]*+|\s++$|\s*[\r\n]|\s+(?!\S)|\s
  ```

  Everything in it except `\p{L}` and `\p{N}` is stdlib: possessive quantifiers (`++`, `?+`, `{m,n}+`)
  and atomic groups landed in **Python 3.11**; `(?i:…)` has always worked. Unicode property escapes
  are the only gap — the `re` docs say so explicitly and point at the third-party `regex` module.
  o200k, by contrast, needs `\p{Lu}`/`\p{Lt}`/`\p{Lm}`/`\p{Lo}`/`\p{Ll}`/`\p{M}` as *separate*
  classes, which is five more hand-built tables and five more chances to drift.

### 6.3 The gap closes, and it was measured today

`\p{L}` and `\p{N}` can be materialised as explicit codepoint-range character classes from
`unicodedata`, which ships with CPython. Experiment run on this box (Python 3.14.4, unicodedata
16.0.0):

- Building both range tables: **0.17 s** (677 `L` ranges, 144 `N` ranges).
- Compiling the resulting 46,897-character pattern with stdlib `re`: **0.01 s**.
- Split output on a JSON-plus-Unicode sample: **identical to the `regex` package** using the literal
  `\p{L}`/`\p{N}` pattern.

Full encoder differential test — a ~60-line stdlib BPE over the vendored `cl100k_base.tiktoken`,
compared token-for-token against `tiktoken` 0.13.0:

| Corpus | Files | Tokens | Mismatches |
|---|---|---|---|
| `docs/**/*.md` + 150 × `crates/**/*.rs` + `instruments/**/*.json` | 283 | **701,625** | **0** |
| Random-Unicode fuzz (seeded, U+0001–U+2FFFF, surrogates excluded) | 3,000 strings | — | **0** |

Speed: pure-Python is ~4× slower than the Rust extension — **0.04 s vs 0.01 s** on 34,436 tokens,
i.e. roughly **1 s per million tokens**. The gate tokenizes a handful of reference solutions, tool
schemas and `instructions` per run. This is free.

Scratch code for the probe is not committed; it exists to justify the claim, and the real
implementation belongs in `eval/` with the differential test against `tiktoken` living in a dev
extra, never in the gate job.

### 6.4 The one trap to avoid

**Do not build the `\p{L}`/`\p{N}` ranges from live `unicodedata` at gate time.** CPython bundles a
Unicode version that changes across releases (this box: Python 3.14.4 / Unicode 16.0.0). A CI runner
upgrading Python would silently move the character classes, silently re-tokenize, and silently
re-baseline the trend on `bench-history` — the exact cross-year incomparability #592 exists to
prevent.

Vendor the ranges as a generated, hash-pinned data file next to the vocab:

```
eval/tokenizer/cl100k_base.tiktoken       # 1,681,126 B, sha256 223921b7…b2a7
eval/tokenizer/unicode_classes.py         # generated L/N ranges, Unicode 16.0.0, pinned
```

and have the guard assert both hashes before it counts a single token. Regenerating
`unicode_classes.py` is then a deliberate, reviewable act with a visible baseline reset — the same
discipline #592 asks for when re-pinning a rung.

---

## 7. Loose ends worth naming

- **The `.tiktoken` data file has no explicit license.** `openai/tiktoken` is MIT (Copyright 2022
  OpenAI, Shantanu Jain), and that covers the code; the encoding files live on
  `openaipublic.blob.core.windows.net` and carry no separate license grant. In practice everyone
  vendors them, but if the repo wants a clean paper trail, the fallback is the GPT-2 BPE files from
  [`openai-community/gpt2`](https://huggingface.co/openai-community/gpt2), which HF tags
  **explicitly `license: mit`** (vocab.json 1.04 MB + merges.txt 456 kB) — at the cost of a
  tokenizer that over-counts JSON.
- **`karpathy/minbpe`** (MIT, pure Python, unmaintained since 2024-07) implements the GPT-4/cl100k
  split pattern and is a useful *reference* for the differential test — but it depends on `regex`,
  so it cannot be the vendored implementation.
- **Ollama's default context is 4k below 24 GiB of VRAM** (docs.ollama.com/context-length). Every
  rung on this ladder is below that threshold, so the harness **must** set `OLLAMA_CONTEXT_LENGTH`
  (or `num_ctx`) explicitly or the 8GB and 16GB rungs will silently truncate the grounding budget and
  fail for the wrong reason.
- **#592's "temperature 0" needs a second look.** Ollama ships `qwen3.5` with `temperature 1`,
  `top_k 20`, `top_p 0.95`, `presence_penalty 1.5`; Qwen's own card recommends a presence penalty to
  stop repetition loops during thinking. Greedy decoding on a thinking model is exactly the regime
  where those loops appear, and Ollama's Go runner discards penalty parameters silently
  (`#14493` Bug 1). Whatever family gets pinned, the harness should record the sampler parameters it
  actually got, not the ones it asked for.
- **The re-pin ritual is still fog**, as #597 says. §4.3 gives a trigger for *when* to re-pin; it
  does not solve *how the live-tier series stays comparable across a re-pin*. The deterministic tier
  is immune by construction (that is why the tokenizer is unrelated to any rung), so the honest
  answer may be that the live-tier series simply breaks at a re-pin and the harness should record a
  visible epoch boundary rather than pretend continuity.

---

## Sources

All accessed 2026-07-22.

**Ollama (registry + docs + tracker):**
- Tool-capable model list — https://ollama.com/search?c=tools ; library — https://ollama.com/library
- Tags + sizes — https://ollama.com/library/qwen3.5/tags , /qwen3.6/tags , /granite4.1/tags , /gemma4/tags , /gpt-oss/tags
- Model pages — https://ollama.com/library/qwen3.5 , https://ollama.com/library/granite4.1
- Manifest/blob digests and byte sizes — `registry.ollama.ai/v2/library/{qwen3.5,granite4.1}/manifests/<tag>`, and the granite4.1 template blob `sha256:89a0ab46e638…`
- Context-length defaults — https://docs.ollama.com/context-length
- OpenAI compatibility (tools over `/v1/chat/completions`) — https://ollama.com/blog/openai-compatibility , https://docs.ollama.com/api/openai-compatibility
- Issue #14493 (OPEN) — https://github.com/ollama/ollama/issues/14493 ; PR #15224 (unmerged) — https://github.com/ollama/ollama/pull/15224 ; issue #14745 (closed 2026-03-27) — https://github.com/ollama/ollama/issues/14745
- Releases v0.31.1 → v0.32.2 — https://github.com/ollama/ollama/releases

**llama.cpp:**
- Issue #20260 (OPEN, peg-native prefix-text parse failure) — https://github.com/ggml-org/llama.cpp/issues/20260
- PR #24329 (merged 2026-06-15, fail-soft hardening) — https://github.com/ggml-org/llama.cpp/pull/24329

**Model cards / registries:**
- https://huggingface.co/Qwen/Qwen3.5-4B , -9B , -27B , -35B-A3B
- https://huggingface.co/api/models?search=Qwen3.5 and ?search=granite-4.1 (downloads, licenses, GGUF file lists, `lastModified`)
- https://ollama.com/library/granite4.1 (Granite 4.1: dense 3B/8B/30B, Apache-2.0, 128K→512K)
- BFCL — https://gorilla.cs.berkeley.edu/leaderboard.html (V4, last updated 2026-04-12) ; https://llm-stats.com/benchmarks/bfcl-v4 (updated 2026-07-22)

**Hardware / speed:**
- Metal working-set cap and `iogpu.wired_limit_mb` — https://github.com/ivanopcode/devnote-override-macos-metal-vram-cap
- https://github.com/stared/benching-local-llms-on-apple-silicon (M5 Max 128GB, 2026-06-14)
- https://antekapetanovic.com/blog/qwen3.5-apple-silicon-benchmark/ (M4 Max 128GB, macOS 26.3)
- https://llmcheck.net/benchmarks (tok/s by chip, Q4_K_M unless noted)

**Tokenizer:**
- https://github.com/openai/tiktoken (MIT LICENSE, README) ; https://raw.githubusercontent.com/openai/tiktoken/main/tiktoken_ext/openai_public.py (patterns + `expected_hash` literals) ; https://raw.githubusercontent.com/openai/tiktoken/main/tiktoken/_educational.py (`import regex`)
- https://pypi.org/pypi/tiktoken/json (0.13.0, `requires_dist: regex, requests`, `>=3.9`)
- Vocab file sizes via `curl -sIL` on `openaipublic.blob.core.windows.net/encodings/{r50k,cl100k,o200k}_base.tiktoken`
- https://docs.python.org/3/library/re.html (possessive quantifiers + atomic groups = 3.11; no `\p{…}`)
- https://huggingface.co/openai-community/gpt2 and /tree/main (license: mit; vocab.json 1.04 MB, merges.txt 456 kB)
- https://github.com/karpathy/minbpe (MIT, `import regex as re`, last push 2024-07-01)

**Experiment (run locally 2026-07-22, scratch, not committed):** stdlib-only cl100k_base
pre-tokenizer + BPE encoder vs `tiktoken` 0.13.0 in a throwaway venv; corpus = this repo's
`docs/**/*.md`, 150 × `crates/**/*.rs`, `instruments/**/*.json` (283 files, 701,625 tokens,
0 mismatches) plus 3,000 seeded random-Unicode strings (0 mismatches). Python 3.14.4,
unicodedata 16.0.0.
