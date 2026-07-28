# ADRs — the live iteration surface

ADRs record architectural decisions **while they're still moving**. Write them normally during
iteration: one decision per file, the usual context / decision / consequences shape. This
directory persists.

Once a decision has solidified, its durable form is a **rule** (+ rationale) under
[`docs/rules/`](../rules/README.md), not an ADR. The `absorb-adrs` skill periodically:

1. distills solidified ADRs into the relevant topic's rules and rationale docs,
2. drops a `Distilled from: ADR-00xx` provenance line into each rationale, and
3. deletes the absorbed ADRs (git keeps the history).

A human runs `absorb-adrs` on a cadence — it is not automatic. Superseded and dead-end ADRs
are culled in the same pass; only the reasoning that still applies survives, in the rationale.

**Numbers are never reused.** Because the fold deletes files, the highest number *in this directory*
is not the highest number ever issued — a new ADR takes the next number after the highest in
`git log --all --diff-filter=A --name-only --format="" -- 'docs/adr/*.md'`, not after the highest
still on disk. A reused number collides with the `Distilled from:` lines the absorbed ADR left
behind, which are the only surviving pointers to it and cannot be disambiguated after the fact.

## Overturning a live rule

Because the fold is periodic and the ADR is immediate, an ADR can leave a rule stating the old now
in the present tense for weeks. So **an ADR that overturns a rule marks that rule in the same
change**:

```md
<a id="whole-document-edit"></a>
### <the rule, untouched>

Superseded by: ADR-0066 (pending absorption)
```

`check_rules_links.py` fails if an ADR names a rule anchor (`<topic>.md#<slug>`, as a link or in
backticks) whose rule carries no marker — and fails again later if a marker names an ADR that has
been absorbed away, so the fold has to clear it. Naming the anchor is what triggers this: nothing
can tell overturning from citing, so an ADR that only wants context names the **topic**, the way
code does.

The marker covers the rule line and nothing else, so it is not the whole obligation: the same claim
is usually restated in the topic's `## Now` prose and its `## Terms` entry, and the `## Terms` entry
is republished into the index glossary by the derive. An ADR that overturns a rule strikes or hedges
those restatements in the same change too — the guard cannot see them, and left alone they state the
old now in the present tense with nothing to notice it.

**Do not** cite ADR numbers from code. Code points at topics: `// see rules: <topic>`. The only
surviving ADR mentions anywhere are (a) `Distilled from:` lines in rationale docs and (b) the
live ADRs here.
