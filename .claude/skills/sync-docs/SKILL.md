---
name: sync-docs
description: Bring reuben's living docs back in sync with the code after a feature lands. Sweeps ARCHITECTURE, README, the authoring guide (docs/agents/authoring.md), docs/agents/operator-dev.md, the reuben-mcp prose strings (once the crate ships), and the skills' workflow sections (patcher, control-surface, create-operator) against the current code + git diff, regenerates the intent-vocabulary and library-index artifacts, verifies the compact describe projection, and flags new domain terms. Use when a feature is implemented, before opening a PR, or when the user says "sync docs", "update the docs", "currentness pass", or "currency pass".
---

# sync-docs

Reuben separates **status** docs (track what's built — drift constantly) from
the **rules** (`docs/rules/` — the ratified now-story + rationale) and the **glossary** (the
derived glossary in `docs/rules/README.md` — deliberate ubiquitous language). This skill
updates the status docs to match reality. It never edits the rules and never invents glossary
terms.

## When to run

After a feature lands (new operator, engine capability, example rig) or just before a PR.
Run from the feature branch so the diff is meaningful.

## Workflow

1. **Find what changed.** `git diff --stat $(git merge-base HEAD main)..HEAD` and read the
   substantive diffs. Identify shipped features: new operators
   (`crates/reuben-core/src/operators/`), new example rigs (`instruments/`), new engine
   capabilities, new tests asserting an invariant.

2. **Sweep each living doc** against what's now true (edit only what drifted):
   - **GitHub issues** — when a feature ships, close (or note progress on) the tracking
     issue. The open work and design backlog live in the issue tracker, not a roadmap doc.
   - **docs/rules/ (the rules corpus)** — the now-based architecture. Clear a topic doc's inline
     "not built yet" flag once a mechanism ships; fix operator counts and name-lists in the topic
     "Now" prose; keep it honest. The topic docs describe the *target* design and flag what isn't
     built inline — don't delete future-tense design, just drop the stale "not yet" qualifier. When
     a decision has fully solidified, the `absorb-adrs` skill (not this one) folds its ADR into a
     rule + rationale.
   - **README.md** — Status section, the example-rig table (new `instruments/*.json` →
     a row; note self-play vs needs-OSC), and Prerequisites if deps changed.
   - **docs/agents/authoring.md** — the instrument-authoring guide (served in-band as
     `reuben://guide/authoring`): the type system and wiring rules, the
     instrument format and `interface` pipes, addressing, the authoring loop, the sample
     workflow, the swap rules of thumb. Sweep when the format, loader, or engine behavior
     changes — this is the one canonical home for those rules.
   - **docs/agents/operator-dev.md** — the operator-development doc: the `Operator` trait,
     `operator_contract!`/Descriptor, registration, the add-an-operator steps, `OpDriver`
     testing, the operator-author RT-safety rules. Sweep when the operator contract, macro
     grammar, or registry changes.
   - **`reuben-mcp` prose strings** (once the crate ships) — the server `instructions`
     paragraph and the tool-description strings. Their posture is **gist-and-point**:
     they never restate contract facts — verify each still gists
     correctly and points at `reuben://guide/authoring`; if one has grown an inline contract
     copy, thin it back to the pointer.
   - **`.claude/skills/*/SKILL.md`** — the three authoring skills keep their **workflow**
     (steps, commands, canonical recipes, scope tables) and point at the canonical docs for
     contract facts (**patcher** → the authoring guide; **create-operator** →
     operator-dev.md; **control-surface** → `surfaces/surface.schema.json`).
     Sweep them for **workflow drift only** — a renamed command, a moved file, a recipe that
     no longer validates; if a skill has re-grown normative contract prose, thin it back to
     its pointer rather than syncing the copy. The other skills don't touch a contract —
     skip them.

3. **Regenerate the generated artifacts** (if their source changed):
   - **intent vocabulary** — after editing `docs/agents/vocabulary.json`, or
     renaming/removing an operator or param a row references: `cargo run -p reuben-core
     --example gen_vocabulary`, then commit `docs/agents/vocabulary.md` if it changed.
     `committed_rendered_view_is_in_sync` and
     `committed_vocabulary_references_the_live_registry`
     (`crates/reuben-core/tests/vocabulary.rs`) fail when it's stale. Rows themselves are
     eval-gated content — sweep the mechanics, never hand-add a word.
   - **library index** — after adding, editing, or removing an
     `instruments/*.json` document: `cargo run -p reuben-native --example
     gen_library_index`, then commit `instruments/index.md` if it changed.
     `library_index_is_in_sync` (`crates/reuben-native/tests/library_index.rs`) fails when
     it's stale.
   - **compact describe** (`describe_compact`, `reuben describe --compact`) —
     nothing to regenerate or commit: it's a live projection of the registry
     (`crates/reuben-core/src/introspect.rs`), proven fresh by
     `describe_compact_lists_exactly_the_registry`. If you touch prose describing the
     operator listing (the rules topic docs, the authoring guide), verify it still gists the
     compact mode rather than restating its shape.

4. **Flag, don't edit, new vocabulary.** If the feature introduces a domain term not in
   the rules glossary (`docs/rules/README.md`), surface it and suggest `/domain-modeling` — the
   glossary is grilled, not auto-written. Likewise, if a change contradicts a rule, surface it;
   don't rewrite the rule.

5. **Verify.** `cargo build` and `cargo test` pass; every doc link/path you touched
   resolves; new instrument names match files in `instruments/`.

## Scope

| Doc | Action |
|-----|--------|
| docs/rules/ (topic docs), README.md, docs/agents/authoring.md, docs/agents/operator-dev.md | **edit** to match reality |
| `reuben-mcp` prose strings (server `instructions` + tool descriptions, once the crate ships) | **verify gist-and-point** — each gists and points at `reuben://guide/authoring`, never restates contract facts |
| `.claude/skills/{patcher,control-surface,create-operator}/SKILL.md` | **edit** for workflow drift only — contract facts live in the canonical docs; leave the other skills alone |
| `docs/agents/vocabulary.json` → `docs/agents/vocabulary.md` (intent vocabulary) | **regenerate** via `gen_vocabulary` — edit the source, never hand-edit the rendered view |
| `instruments/*.json` → `instruments/index.md` (library index) | **regenerate** via `gen_library_index` — edit an instrument, never hand-edit the index |
| compact describe projection (`describe_compact`) | **verify** — a live registry projection, no committed file; nothing to regenerate |
| the rules glossary (`docs/rules/README.md`) | **flag** new terms → suggest /domain-modeling, don't auto-edit |
| docs/rules/* | **never touch** — the ratified now-rules, not status |

## Report

End with: which docs changed and why, which generated artifacts were regenerated
(vocabulary, library index — yes/no each), and any flagged terms or ADR conflicts left for
the user to decide.
