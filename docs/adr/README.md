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

**Do not** cite ADR numbers from code. Code points at topics: `// see rules: <topic>`. The only
surviving ADR mentions anywhere are (a) `Distilled from:` lines in rationale docs and (b) the
live ADRs here.
