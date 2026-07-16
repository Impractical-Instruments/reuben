# ADR-0058: Intent vocabulary — one curated word→move table, three delivery lanes

## Status

Accepted (2026-07-16). The intent→parameter vocabulary decision of the patch-pipeline
streamlining effort — wayfinder ticket
[Patch-pipeline/G (reuben-web#88)](https://github.com/Impractical-Instruments/reuben-web/issues/88)
on map [reuben-web#81](https://github.com/Impractical-Instruments/reuben-web/issues/81),
grounded on
[Patch-pipeline/C's prior-art survey](https://github.com/Impractical-Instruments/reuben-web/blob/dev/docs/research/patch-pipeline-intent-prior-art.md).
**Rides on** [ADR-0045](0045-whole-document-edit-contract.md) (whole-document edits — the
in-context join below depends on the agent holding the full document),
[ADR-0051](0051-authoring-grounding-single-source.md) (single-source grounding; §4's
read-at-request-time posture), and [ADR-0057](0057-instrument-reuse-interface-makes-the-role.md)
(instrument reuse; face pipes). **Amends** [ADR-0048](0048-mcp-tool-surface-and-contracts.md) §7
by exactly one resource (§6 below).

## Context

- A patching agent turning "make it warmer / busier / sadder" into parameter moves needs the
  word→move mapping stated somewhere. Prior art (Patch-pipeline/C) shows every published
  mapping binds to a *fixed* parameter space (Audealize's 40-band EQ, SAFE's five bands,
  LLM2Fx's DASP chain); reuben's target is an arbitrary operator graph, so what transfers is
  the word→parameter-**move** form, not the word→parameter-vector form. C's strongest result
  (LLM2Fx, WASPAA 2025): a model with the mapping **in its prompt, zero-shot,** beat
  CLAP-based optimization — in-prompt grounding is the validated form.
- Word traffic is heavily cliffed (SAFE: warm 582, bright 531, third place 34; vendor knobs
  converge on ~7 words), so the vocabulary can be small with confidence.
- Three consumer lanes exist and must all be served (decided on the map, 2026-07-16): the
  web chat agent (grounding must be **gist-complete** — it has no checkout and no resources),
  MCP clients (in-band resources are their only grounding), and the repo skills
  (gist-and-point works).
- The engine already carries a first-class tonal subsystem (`vocab/harmony.rs`; the
  `harmony`, `chord`, `snap`, `transpose`, `strum` operators), so tonal words have discrete,
  defensible canonical moves.

## Decision

### 1. Form: one canonical grounding doc — a curated descriptor→move table

The vocabulary layer is a **document**, not a lookup tool and not per-instrument
annotations. A tool call prices the mapping into the volatile tail and hides it until asked;
per-instrument annotations mint N drift pairs and cover only annotated instruments. The
table rides in the agent's stable grounding.

### 2. Binding: instrument-blind, registry-keyed; the join happens in context

Rows key on **operator types + input names only** — the registry-owned vocabulary. The
table never names an instrument, a file, or a pipe. The join to the concrete instrument
happens at patch time: under ADR-0045 the agent holds the whole document, whose nodes name
their operator types; the model matches table rows to nodes in context. Two corollaries:

- **Canonical pipe naming is a recipe-authoring guideline**: a nested instrument's face
  pipes (ADR-0057) use the same names the table's moves target (`cutoff`, `tone`, `decay`,
  `drive`, …) wherever they proxy that move, so type-keyed vocabulary transfers to faces by
  name. Binds the seed six; checkable in evals, not mechanically provable.
- **Instrument-specific vocabulary, if ever needed, lives in that instrument's own `doc`
  lines** — atomic with the file, never in the central table.

**Considered and rejected:** a second table section keyed to canonical pipe names
(duplicates rows per word and has no registry to sweep against — unsweepable prose).

### 3. Day-one contents: three sections + a fallback block, table-only

- **Timbral** (~10–15 words): transcribed from the published semantic-differential data
  (Audealize/SAFE) and DAW macro conventions, re-keyed to reuben operator types.
- **Rhythmic** (~8–12 words): busier/sparser, swung/straight, longer/shorter, tighter/looser
  — corpus- and judgment-sourced; prior art is timbral-only but the corpus is majority
  rhythm plumbing, so the table must not be mute on it.
- **Tonal** (3–6 words): sadder/happier (scale/mode), richer (chord size), dissonance (snap
  policy) — discrete moves on the harmony subsystem. Overloaded words (*darker*) carry
  explicit cross-section disambiguation conditions; a condition can only point at a move
  that exists, which is why tonal ships day one rather than later.

Each row: word, ordered moves `{operator type, input, direction, magnitude, condition}`,
antonym pointer. ~20–28 words total.

The **fallback block** (10–20 direction-only lines, ~250 tokens) grounds words outside the
table — high-frequency-energy words → brightness-family moves, size words → space/detune,
etc. — plus the tie-breaker: *table wins when the word is in the table; otherwise one
conservative move, then let the user react* (the act-then-react posture).

**No worked exemplars day one.** Exemplars are the named hardening tier: fragment-level
before/after (one node, ~150–300 tokens — never whole-document, the protocol is taught
once elsewhere), entering **per word, only on a named eval failure**. Growth of everything
— rows, sections, exemplars — is eval-gated; no speculative spend.

### 4. Authorship: structured source, agent-drafted, human-locked

The source of truth is a **structured file** (one entry per word: section, moves, antonyms,
conditions); the prompt sees a compact **generated rendered view**. A build ticket drafts
the rows by mining the published data, the idiom memo, and the live registry; the human
reviews and locks every row. After that the file is curation, not a pipeline output.

**Considered and rejected:** authoring directly in prose (forfeits the mechanical staleness
test); building the table from learned/CLAP machinery (C ruled it out — offline
table-builder at most, not the starting form).

### 5. Freshness: referential by CI, musical by evals

- **Referential**: a staleness test parses every move and asserts its operator type + input
  exist in the registry (the ADR-0057 index-test pattern). An operator rename breaks the
  build, not the agent.
- **Musical**: whether "warmer" still does the right thing is judged by synthetic-session
  evals (dev/synthetic only — ADR-0006's privacy posture). No mechanical check can own it.

### 6. Home and delivery: engine-canonical; one artifact, three lanes

The structured source, generator, rendered view, and staleness test live **in this repo**,
next to the registry that sweeps them (the `gen_schema` posture: one source, generated
artifact, CI-checked). Delivery per lane:

- **Skills**: gist-and-point at the rendered doc (ADR-0051 §3, unchanged).
- **MCP**: the sidecar serves the rendered view as **`reuben://guide/vocabulary`** — a
  one-resource amendment to ADR-0048 §7's fixed surface — read from the checkout at request
  time per ADR-0051 §4 (no `include_str!`; a stale binary cannot serve a stale table). The
  `reuben-mcp` prose strings gist-and-point at it; the resource joins `sync-docs`' sweep.
- **Web**: the rendered view is bundled into the web agent's prompt at build time
  (gist-complete — there is nothing to point at).

**Considered and rejected:** a section of `reuben://guide/authoring` (splices a generated
artifact into a hand-maintained file — a mixed-ownership seam that drifts); web-side home
(splits the table from its staleness test and its other two lanes).

## Consequences

- The registry gains a second curated-but-swept satellite artifact alongside the generated
  schema; CI owns its referential truth.
- ADR-0048 §7's resource surface is amended to exactly three resources: schema, authoring
  guide, vocabulary. `sync-docs`' scope grows by the vocabulary artifact and the pointer
  prose.
- The seed-recipe work inherits the canonical pipe-naming guideline (§2); the build spec
  must state it.
- The table also serves the **reverse translation** — explaining an applied change back to
  the user in the user's own vocabulary (the change-card surface).
- Out-of-band investigations filed on the product repo: passive detection of out-of-table
  words ([reuben-web#100](https://github.com/Impractical-Instruments/reuben-web/issues/100))
  and of operator gaps
  ([reuben-web#101](https://github.com/Impractical-Instruments/reuben-web/issues/101)) —
  both privacy-gated by ADR-0006.
- Exact file paths, formats, and the eval harness are build-spec decisions
  (Patch-pipeline/H), not fixed here.
