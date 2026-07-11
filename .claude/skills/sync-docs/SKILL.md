---
name: sync-docs
description: Bring reuben's living docs back in sync with the code after a feature lands. Sweeps ARCHITECTURE, README, the authoring guide (docs/agents/authoring.md), docs/agents/operator-dev.md, the reuben-mcp prose strings (once the crate ships), and the skills' workflow sections (patcher, control-surface, create-operator) against the current code + git diff, regenerates the instrument schema, and flags new domain terms. Use when a feature is implemented, before opening a PR, or when the user says "sync docs", "update the docs", "currentness pass", or "currency pass".
---

# sync-docs

Reuben separates **status** docs (track what's built — drift constantly) from
**decision** docs (ADRs — frozen rationale) and the **glossary** (CONTEXT.md — deliberate
ubiquitous language). This skill updates the status docs to match reality. It never edits
ADRs and never invents glossary terms.

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
   - **docs/ARCHITECTURE.md** — clear inline "not built yet" / "isn't built yet" flags once
     a mechanism ships; fix operator counts and name-lists; keep the status line honest.
     This doc describes the *target* design — don't delete future-tense design, just drop
     the stale "not yet" qualifier.
   - **README.md** — Status section, the example-rig table (new `instruments/*.json` →
     a row; note self-play vs needs-OSC), and Prerequisites if deps changed.
   - **docs/agents/authoring.md** — the instrument-authoring guide (served in-band as
     `reuben://guide/authoring`, ADR-0048 §7): the type system and wiring rules, the
     instrument format and `interface` pipes, addressing, the authoring loop, the sample
     workflow, the swap rules of thumb. Sweep when the format, loader, or engine behavior
     changes — this is the one canonical home for those rules.
   - **docs/agents/operator-dev.md** — the operator-development doc: the `Operator` trait,
     `operator_contract!`/Descriptor, registration, the add-an-operator steps, `OpDriver`
     testing, the operator-author RT-safety rules. Sweep when the operator contract, macro
     grammar, or registry changes.
   - **`reuben-mcp` prose strings** (once the crate ships) — the server `instructions`
     paragraph and the tool-description strings. Their posture is **gist-and-point**
     (CONTEXT.md; ADR-0051 §4): they never restate contract facts — verify each still gists
     correctly and points at `reuben://guide/authoring`; if one has grown an inline contract
     copy, thin it back to the pointer.
   - **`.claude/skills/*/SKILL.md`** — the three authoring skills keep their **workflow**
     (steps, commands, canonical recipes, scope tables) and point at the canonical docs for
     contract facts (**patcher** → the authoring guide; **create-operator** →
     operator-dev.md; **control-surface** → `surfaces/surface.schema.json` + ADR-0043).
     Sweep them for **workflow drift only** — a renamed command, a moved file, a recipe that
     no longer validates; if a skill has re-grown normative contract prose, thin it back to
     its pointer rather than syncing the copy. The other skills don't touch a contract —
     skip them.

3. **Regenerate the schema** (if any operator/param changed):
   `cargo run -p reuben-core --example gen_schema`, then commit
   `crates/reuben-core/schema/instrument.schema.json` if it changed. The
   `committed_schema_is_in_sync` test fails when it's stale.

4. **Flag, don't edit, new vocabulary.** If the feature introduces a domain term not in
   CONTEXT.md, surface it and suggest `/domain-modeling` — the glossary is grilled, not
   auto-written. Likewise, if a change contradicts an ADR, surface it; don't rewrite the ADR.

5. **Verify.** `cargo build` and `cargo test` pass; every doc link/path you touched
   resolves; new instrument names match files in `instruments/`.

## Scope

| Doc | Action |
|-----|--------|
| ARCHITECTURE.md, README.md, docs/agents/authoring.md, docs/agents/operator-dev.md | **edit** to match reality |
| `reuben-mcp` prose strings (server `instructions` + tool descriptions, once the crate ships) | **verify gist-and-point** — each gists and points at `reuben://guide/authoring`, never restates contract facts |
| `.claude/skills/{patcher,control-surface,create-operator}/SKILL.md` | **edit** for workflow drift only — contract facts live in the canonical docs; leave the other skills alone |
| instrument schema | **regenerate** via gen_schema |
| CONTEXT.md (glossary) | **flag** new terms → suggest /domain-modeling, don't auto-edit |
| docs/adr/* | **never touch** — decisions, not status |

## Report

End with: which docs changed and why, schema regenerated (yes/no), and any flagged terms
or ADR conflicts left for the user to decide.
