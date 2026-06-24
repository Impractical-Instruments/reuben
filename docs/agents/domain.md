# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring
the codebase. reuben is a **single-context** repo: one `CONTEXT.md` and one `docs/adr/`
tree, both at the root.

## Before exploring, read these

- **`CONTEXT.md`** at the repo root — the glossary / ubiquitous language.
- **`docs/adr/`** — read the ADRs that touch the area you're about to work in.

If either is missing, **proceed silently** — don't flag the absence or suggest creating it.
The `/domain-modeling` skill creates these lazily, when terms or decisions actually get
resolved.

## File structure

```
/
├── CONTEXT.md
├── docs/
│   ├── adr/
│   │   ├── 0001-unified-block-graph-execution.md
│   │   ├── 0003-recursive-composition.md
│   │   └── 0007-osc-only-core.md
│   └── agents/
└── crates/
```

## Use the glossary's vocabulary

When your output names a domain concept (an issue title, a refactor proposal, a hypothesis,
a test name), use the term as defined in `CONTEXT.md` — Operator, Instrument, Rig, Plan,
Swap, Lane, Voice, and so on. Don't drift to the synonyms the glossary explicitly tells you
to avoid (e.g. "node", "module", "patch" as a noun).

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing
language the project doesn't use (reconsider) or there's a real gap (note it for
`/domain-modeling`).

## Flag ADR conflicts

If your output contradicts an existing ADR, surface it explicitly rather than silently
overriding:

> _Contradicts ADR-0007 (OSC-only core) — but worth reopening because…_
