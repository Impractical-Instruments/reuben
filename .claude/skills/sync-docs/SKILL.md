---
name: sync-docs
description: Bring reuben's living docs back in sync with the code after a feature lands. Sweeps ARCHITECTURE, README, and docs/agents/authoring.md against the current code + git diff, regenerates the instrument schema, and flags new domain terms. Use when a feature is implemented, before opening a PR, or when the user says "sync docs", "update the docs", "currentness pass", or "currency pass".
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
   - **docs/agents/authoring.md** — the operator list and the add-an-operator steps if the
     contract or registry changed.

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
| ARCHITECTURE.md, README.md, docs/agents/authoring.md | **edit** to match reality |
| instrument schema | **regenerate** via gen_schema |
| CONTEXT.md (glossary) | **flag** new terms → suggest /domain-modeling, don't auto-edit |
| docs/adr/* | **never touch** — decisions, not status |

## Report

End with: which docs changed and why, schema regenerated (yes/no), and any flagged terms
or ADR conflicts left for the user to decide.
