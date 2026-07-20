---
name: absorb-adrs
description: Distill solidified ADRs into now-rules + rationale under docs/rules/, then delete the absorbed ADRs — the durable engine of the ADR→rules pipeline. Reads an ADR and its full supersession chain, finds the current "now", writes a present-tense rule + a condensed rationale (with a `Distilled from:` provenance line) conforming to the docs/rules/ conventions, harvests useful code-comment reasoning, regenerates the derived index, and runs the guards green. Use when a human says "absorb the ADRs", "run the ADR sweep", "turn these ADRs into rules", "distill ADR-00xx", or on the periodic rules-doc maintenance cadence.
---

# absorb-adrs

ADRs are the **live iteration surface** — one decision per file, written while the decision is still
moving ([`docs/adr/README.md`](../../../docs/adr/README.md)). Once a decision has **solidified**, its
durable form is a **rule** (+ rationale) under [`docs/rules/`](../../../docs/rules/README.md), not an
ADR. This skill is the transform: it reads solidified ADR(s), distills the current "now" into rules
and condensed rationale, harvests the reasoning that still applies, regenerates the derived index,
and **deletes the absorbed ADR files** (git keeps the history).

Read the S01 conventions before you touch anything — they are canonical, this skill only drives them:
[`docs/rules/README.md`](../../../docs/rules/README.md) (the ladder + conventions),
[`docs/rules/_templates/{topic,rule,rationale}.md`](../../../docs/rules/_templates), and
[`docs/adr/README.md`](../../../docs/adr/README.md) (the lifecycle this skill implements). This is a
**human-triggered** skill — a person runs it on a cadence and picks which ADRs are ripe; it is never
automatic.

**Portability.** Everything below is repo-relative (`docs/rules/`, `docs/adr/`,
`docs/rules/_templates/`, `scripts/check_rules_*.py`, and this skill's own `scaffold_rule.py`). The
same layout exists in the web repo, so this skill runs unchanged there — never hardcode an
engine-only path.

## The topic map (ratified taxonomy)

Every rule lives under exactly one of six topics. The slugs are **stable kebab-case** — the same run
after run, so `see rules:` code comments and cross-repo links never move. Use these; do not invent a
new topic without a taxonomy change.

| Topic | Slug | Covers |
|---|---|---|
| **T1** Execution & runtime | `execution-runtime` | Plan lifecycle, RT boundary, determinism, scheduling/threading, swap, latch service, embed surface. |
| **T2** Composition & operator model | `composition-operators` | The one recursive graph — operator contract/registration, values vs signals, the `Message`/`Arg` substrate, nesting, interface pipes. |
| **T3** Signal / OSC / time / DSP | `signal-time-dsp` | OSC-only message model, clock & musical time, pitch & tuning, tonal context, DSP families (envelopes, math). |
| **T4** Authoring surface & instrument library | `authoring-library` | Control surface, decoupled surface docs, sample/resource store, library resolution & format versioning, the Toys. |
| **T5** Agent framework & MCP | `agent-mcp` | AI-authorability, introspection + authoring skills, the MCP sidecar / tool contracts, grounding single-source, intent vocabulary. |
| **T6** Web/product boundary & dev process | `web-product-process` | The C-ABI web boundary, SDK-vs-private-product split, share links, branch/release strategy, toolchain pin, perf-benchmark CI. |

Which ADR maps to which topic (and its supersession state) is the **crosswalk** the sweep produces —
per run, the human hands you the ADR→topic assignment; the topic doc it lands in is one of the six
above. A topic's `## Rules` may hold many rules from many ADRs.

## The sweep: which ADRs are "solidified" enough to absorb

Absorbing is **lossy on purpose** — dead-end history is dropped (git keeps it). So only absorb a
decision that has stopped moving. Before absorbing an ADR, confirm **all** of:

- **No open supersession or iteration.** It is not itself marked provisional/draft, and nothing
  newer is actively revising it. A `sup=part` ADR is fine to absorb *as long as* the surviving
  decision is stable — you distill the part that still holds, not the retired mechanism.
- **The decision has held across time / downstream ADRs.** Later ADRs build on it rather than
  re-litigate it. A brand-new ADR that nothing has stress-tested yet stays an ADR.
- **Not referenced as "provisional"/"to be revisited"** by itself or its neighbours.
- **`FULL`-superseded ADRs** are absorbed as culls, not authored rules: their "now" is often a single
  line that the decision was reversed, captured in the *superseding* rule's rationale (or dropped
  entirely if nothing survives) — then the file is deleted. Do not mint a rule for a dead decision.

When in doubt, **leave it as an ADR** — the sweep is periodic; a not-yet-ripe decision gets absorbed
next pass. Absorb in small batches (one ADR or a tight same-topic cluster) so each PR is reviewable.

## The procedure (per ADR or small same-topic cluster)

Run everything from the repo root.

1. **Read the ADR(s) + the full supersession chain.** Open the target ADR and every ADR it
   supersedes / is superseded by / amends (follow the `Superseded by` / `Supersedes` / `Amends` links
   at the top and in Consequences). Determine the **current "now"** — the position that holds today —
   and **discard dead-end history**: earlier drafts, rejected alternatives, retired mechanisms. A
   `sup=part` ADR keeps only its surviving decision; a `FULL`-superseded ADR usually contributes
   nothing but a cull (see the sweep note).

2+3. **Scaffold the rule + its rationale, then fill them.** For each distinct normative decision,
   pick a **stable kebab-case rule slug** naming the *concept* (so the sentence can be reworded
   without breaking the anchor), then scaffold the guard-safe skeleton:

   ```
   python3 .claude/skills/absorb-adrs/scaffold_rule.py \
     --topic <topic-slug> --title "<Topic Title>" --summary "<one-line topic summary>" \
     --rule <rule-slug> --heading "<present-tense normative statement.>" \
     --from "ADR-00xx[, ADR-00yy]"
   ```

   This creates `docs/rules/<topic>.md` (as a skeleton mirroring `_templates/topic.md`) if it does
   not exist, appends the rule block (`<a id>` + `### heading` + exactly one
   `[why](rationale/<topic>/<slug>.md)` link) into `## Rules`, and instantiates
   `docs/rules/rationale/<topic>/<slug>.md` from `_templates/rationale.md` with the `Distilled from:`
   line filled. It refuses to clobber a rationale or duplicate a rule slug, so re-running is safe.
   Then **you** write the judgement the helper can't:
   - the **rule heading** — one present-tense normative sentence (already passed via `--heading`;
     refine in the doc if needed);
   - the **rationale body** — the condensed reasoning that *still applies* (replace the `TODO` line).
     Keep the forces that make the rule the right call; drop superseded alternatives and dead-end
     history. As long as it needs to be, no longer.

   **Strictly 1:1 rule↔rationale** (S01 amendment): never point two rules at one rationale file. If
   two rules genuinely share a why, either merge them into one rule or give each its own rationale and
   cross-link with a "see also" — never share the file. The helper enforces this structurally (one
   `[why]` per rule, unique slug), but the *content* discipline is yours.

4. **Harvest code-comment rationale.** Skim the code the ADR governs for inline comments that carry
   genuine *why* (an invariant, a subtle trade-off, a "we do it this way because…"). Fold that
   reasoning into the rationale doc so it survives. **Do not repoint the comments here** — rewriting
   them to the `// see rules: <topic>` form is a later comment-stage job; this step only rescues the
   reasoning into the rationale.

5. **Update the topic's `## Now` / `## Terms`, then regenerate the derived index.** Replace the
   skeleton's `## Now` TODO with the present-tense "now" story for the topic (prose, orienting, not a
   rule list), and add any defining **Terms** (`- **Term** — definition`; each term unique across all
   topics — the derive guard errors on a duplicate). Then regenerate the README's derived
   Topics/Glossary sections:

   ```
   python3 scripts/check_rules_derive.py --write .
   ```

   **Never hand-edit** README's `## Topics` / `## Glossary` bodies — they are derived from the topic
   docs. (The pre-commit hook runs this `--write` automatically on any `docs/rules/` commit; running
   it yourself keeps the tree clean before you self-check.)

6. **Delete the absorbed ADR file(s).** `git rm docs/adr/00xx-*.md` for each ADR fully distilled
   (including `FULL`-superseded culls). The `Distilled from:` line in the rationale is now the only
   surviving pointer to the ADR number; git history keeps the rest.

7. **Self-check — both guards green before you're done:**

   ```
   python3 scripts/check_rules_links.py .          # every topic has ≥1 rule; every rule → 1 existing rationale
   python3 scripts/check_rules_derive.py --check .  # README's derived sections match the topic docs
   ```

   Both must exit 0. If `--check` reds, you edited a topic doc but didn't re-run `--write` (step 5).
   If links reds, a `[why]` target is missing or a rule has ≠1 `[why]` — re-scaffold rather than
   hand-patch.

## Scope

| Thing | Action |
|---|---|
| Solidified ADR → rule + rationale under `docs/rules/` | **author** (distill the "now", write the rule + condensed why) |
| Topic doc `## Now` / `## Terms`, README derived index | **update** `## Now`/`## Terms` by hand; **regenerate** the index via `check_rules_derive.py --write` (never hand-edit derived sections) |
| Absorbed ADR files | **delete** (`git rm`) once distilled |
| Code-comment *reasoning* in the ADR's area | **harvest** into the rationale (do not repoint comments — a later stage) |
| Still-moving / provisional / unripe ADRs | **leave** — absorb next pass |
| New taxonomy / a 7th topic | **never** — the six topics are ratified; a change is its own decision |
| Writing new ADRs, or engine/product code | **never** — this skill only distills existing ADRs |

## Report

End with: which ADR(s) you absorbed and into which topic(s); the rule slug(s) + one-line statement
each; confirmation the two guards exited 0; and the ADR file(s) deleted. Flag any ADR you judged
**not** ripe and left in place, and any place two decisions were close enough that you had to choose
merge-vs-two-rules.
