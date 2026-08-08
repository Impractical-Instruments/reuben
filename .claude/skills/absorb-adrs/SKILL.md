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
`docs/rules/_templates/`, `scripts/check_rules_*.py`, and this skill's own `scaffold_rule.py`) —
never hardcode an engine-only path.

## The topic map

Every rule lives under exactly one topic, and the topic set is **open** — it shifts and grows as the
system does. Never work from a remembered list. The current topics are the topic docs in
[`docs/rules/`](../../../docs/rules), each with its one-line summary collated into the derived
`## Topics` index of [`docs/rules/README.md`](../../../docs/rules/README.md#topics) — read that index
first, then use an existing topic's kebab-case slug wherever the rule fits one. Each index entry
shows a **title**; its link target minus `.md` — the topic doc's filename — **is** that topic's
slug. When none fits, surface the new topic to the human running the sweep — they own the
crosswalk — before scaffolding it.

A slug, once chosen, is **stable** — the same run after run — so `see rules:` code comments and
cross-repo links never move. A topic's title and summary can be reworded freely; its slug cannot.

Which ADR maps to which topic (and its supersession state) is the **crosswalk** the sweep produces —
per run, the human hands you the ADR→topic assignment. A topic's `## Rules` may hold many rules from
many ADRs.

## The sweep: which ADRs are "solidified" enough to absorb

Absorbing is **lossy on purpose** — dead-end history is dropped (git keeps it). So only absorb a
decision that has stopped moving. Before absorbing an ADR, confirm **all** of:

> `sup=part` / `FULL`-superseded below is the annotation shorthand from the (throwaway) #167
> crosswalk, **not** a literal field in any `docs/adr/` file — ADRs record supersession as free-form
> "Superseded by X — …" prose, so read the chain, don't grep for a `sup=` tag.

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

   The helper always writes the `Distilled from:` form, which is right for anything absorbed from an
   ADR. A rule that never passed through one — settled directly in an issue or a PR — ends
   `Decided in: issue #NNN — settled directly, no ADR.` instead; leave those alone on a sweep rather
   than back-filling an ADR number to make them match.

   **Strictly 1:1 rule↔rationale** (S01 amendment): never point two rules at one rationale file. If
   two rules genuinely share a why, either merge them into one rule or give each its own rationale and
   cross-link with a "see also" — never share the file. The helper enforces this structurally (one
   `[why]` per rule, unique slug), but the *content* discipline is yours.

4. **Harvest code-comment rationale, and repoint what you harvested.** Skim the code the ADR governs
   for inline comments that carry genuine *why* (an invariant, a subtle trade-off, a "we do it this
   way because…"). Fold that reasoning into the rationale doc so it survives — then, **in the same
   pass, replace each comment you took it from** with a pointer to the topic. Leaving the comment
   behind is what manufactures restatement: the argument now lives in two places, and the copy in the
   code is the one that drifts silently while the build stays green. The repointing reaches exactly
   the comments this run harvested from — not every comment in the module, not every module the topic
   governs.

   The pointer names a **topic and nothing else** — `see rules: <topic>`, written in whatever comment
   syntax the file already uses (`//`, `#`, `/* … */`). Carrying the slashes into a Python or YAML
   file makes a line that is not a comment at all: a syntax error, and one no guard here can see.
   Never a rule anchor, never an issue number, never the ADR number the rationale was distilled from
   — `check_rules_refs.py` bans all three in a comment, because code points at topics only and a rule
   reworded next sweep must not break a pointer. The issue number is the one a harvest trips most
   often: a rescued "we do it this way because of `#<nn>`" comment carries its provenance with it, and
   provenance belongs in the rationale you just wrote.

   **Which form you write is decided by the repo you are standing in, never by the topic.** Working
   here it is always `see rules: <topic>`, resolved against this repo's own `docs/rules/`. The
   `see engine rules: <topic>` form belongs to a consuming repo that pins this one as a submodule,
   where it resolves against the pinned corpus at `engine/docs/rules/<topic>.md`; this repo has no
   such path, so a session working here never writes that form — the guard reds on every one of them.
   The two are not interchangeable spellings of one pointer: each resolves against a different
   corpus, and the two corpora carry their own slug sets.

   **No deletion authority.** Restatement you merely walked past — a comment this run took nothing
   from — is left exactly as it stands. It is pre-existing, it is owned by the whole-workspace comment
   sweep (#638), and a deletion made inside an ADR-absorption PR arrives in front of a reviewer
   reading rules rather than code. This step repoints the duplicate it just created, and stops there.

5. **Update the topic's `## Now` / `## Terms`, then regenerate the derived index.** Replace the
   skeleton's `## Now` TODO with the present-tense "now" story for the topic (prose, orienting, not a
   rule list), and add any defining **Terms** (`- **Term** — definition`; each term unique across all
   topics — the derive guard errors on a duplicate). Then regenerate the README's derived
   Topics/Glossary sections:

   ```
   python3 scripts/check_rules_derive.py --write .
   ```

   **Never hand-edit** README's `## Topics` / `## Glossary` bodies — they are derived from the topic
   docs. (`.githooks/pre-commit.d/30-rules-index` runs this `--write` automatically on any
   `docs/rules/` commit — in a clone where `scripts/install-hooks.sh` has been run; running it
   yourself keeps the tree clean before you self-check, and is the only guarantee if it has not.)

6. **Delete the absorbed ADR file(s), and clear the markers they placed.** `git rm
   docs/adr/00xx-*.md` for each ADR fully distilled (including `FULL`-superseded culls), then drop
   every `Superseded by: ADR-00xx (pending absorption)` line naming one of them — the rule you just
   rewrote no longer has a pending anything. The `Distilled from:` line in the rationale is now the
   only surviving pointer to the ADR number; git history keeps the rest. The links guard fails on a
   marker naming a deleted ADR, so step 7 catches this if you forget.

7. **Self-check — all three guards green before you're done:**

   ```
   python3 scripts/check_rules_links.py .           # every topic has ≥1 rule; every rule → 1 existing rationale
   python3 scripts/check_rules_derive.py --check .  # README's derived sections match the topic docs
   python3 scripts/check_rules_refs.py .            # no ADR number left in code; every pointer resolves and stops at its topic
   ```

   All three must exit 0. If `--check` reds, you edited a topic doc but didn't re-run `--write`
   (step 5). If links reds, a `[why]` target is missing or a rule has ≠1 `[why]` — re-scaffold rather
   than hand-patch. If refs reds, it is step 4's repointing: a pointer that named a rule anchor, an
   issue number or an ADR number instead of a topic; a slug with no `docs/rules/<topic>.md` behind
   it; the `see engine rules:` form, which resolves nowhere in this repo; or a capitalised
   `See rules:` opening a sentence — the grammar is lowercase, and only that spelling is validated.

## Scope

| Thing | Action |
|---|---|
| Solidified ADR → rule + rationale under `docs/rules/` | **author** (distill the "now", write the rule + condensed why) |
| Topic doc `## Now` / `## Terms`, README derived index | **update** `## Now`/`## Terms` by hand; **regenerate** the index via `check_rules_derive.py --write` (never hand-edit derived sections) |
| Absorbed ADR files | **delete** (`git rm`) once distilled |
| Code-comment *reasoning* in the ADR's area | **harvest** into the rationale, then **repoint** — in the same pass, the comments it came from, to `see rules: <topic>`; those only |
| Still-moving / provisional / unripe ADRs | **leave** — absorb next pass |
| A rule that fits no existing topic | **scaffold** — the topic set is not closed; steps 2+3 create `docs/rules/<topic>.md` on demand, and the slug you pick is stable from then on |
| Writing new ADRs, or engine/product code | **never**, with one exception — step 4's repointing of the comments this run harvested from, and nothing beyond it |

## Report

End with: which ADR(s) you absorbed and into which topic(s); the rule slug(s) + one-line statement
each; the comments you repointed; confirmation the three guards exited 0; and the ADR file(s)
deleted. Flag any ADR you judged
**not** ripe and left in place, and any place two decisions were close enough that you had to choose
merge-vs-two-rules.
