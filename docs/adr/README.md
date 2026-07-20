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

**Do not** cite ADR numbers from code. Code points at topics: `// see rules: <topic>`. The only
surviving ADR mentions anywhere are (a) `Distilled from:` lines in rationale docs and (b) the
live ADRs here.
