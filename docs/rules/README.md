# reuben rules index

The now-state architecture, as rules. Read top-down and **stop at the shallowest level
that answers your question**:

    index (this file)  →  topic doc   →  a rule         →  its rationale
    summaries+glossary     now-story +    present-tense     condensed "why",
                           its rules      statement         read only when needed

- A **topic** is one area of the system: its "now" story plus the rules that hold there.
- A **rule** is a present-tense normative statement with a stable anchor and one rationale link.
- A **rationale** is the condensed "why", loaded only when needed. Provenance lives there.

Code points at topics, never at rules or ADRs: `// see rules: <topic>` (this repo),
`// see engine rules: <topic>` (web → engine). See [Conventions](#conventions).

## Topics

<!-- derived — collated from each topic's `> summary`; do not hand-edit out of sync. -->

## Glossary

<!-- derived — collated from each topic's `## Terms`, linking the defining topic. -->

## Conventions

**Layout**

```
docs/rules/README.md                     index: topic summaries + derived glossary
docs/rules/<topic>.md                    now-story + rules; each rule links its rationale
docs/rules/rationale/<topic>/<rule>.md   condensed why + "Distilled from: ADR-00xx"
docs/adr/                                live ADRs (iteration surface); see docs/adr/README.md
```

**Rule** — a present-tense normative statement, one sentence. Carries a stable kebab-case
slug (unique within its topic) as a raw-HTML `<a id>` anchor above the heading, so the
sentence can be reworded without breaking links. Exactly one rationale link.

**Rationale** — the condensed "why" that still applies; superseded/dead-end paths are dropped
(git keeps them). Ends with a `Distilled from: ADR-00xx[, ADR-00yy]` provenance line. One file
per rule at `rationale/<topic>/<rule>.md`.

**Code-comment reference** — topic-level only, never a rule slug or ADR number:
`// see rules: <topic>` in-repo, `// see engine rules: <topic>` cross-repo. Grammar:
`/\bsee (engine )?rules: ([a-z0-9-]+)/`; the slug must resolve to a topic doc.

**Progressive-disclosure ladder** — index → topic → rule → rationale. Stop at the shallowest
level that answers the question; open a rationale only when you need the why.

**Derived index** — the Topics list and Glossary above are collated from the topic docs; do not
hand-edit them. The `pre-commit` hook regenerates them (`check_rules_derive.py --write`) whenever a
commit touches `docs/rules/`, and CI runs `--check` as a backstop. Run `scripts/install-hooks.sh`
once per clone.

**ADR lifecycle** — see [docs/adr/README.md](../adr/README.md).
