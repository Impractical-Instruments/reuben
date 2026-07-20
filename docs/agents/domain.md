# Domain Docs

How the engineering skills should consume this repo's domain documentation when exploring
the codebase. reuben's now-state architecture lives as a **rules system** under
[`docs/rules/`](../rules/README.md): one index, per-topic docs, individual rules, and a
condensed rationale behind each rule.

## Before exploring, read these

Read top-down and **stop at the shallowest level that answers your question**:

```
docs/rules/README.md   →  docs/rules/<topic>.md  →  a rule       →  its rationale
index: topic summaries    the topic's "now"          present-tense    condensed "why",
+ derived glossary        story + its rules          normative        read only when needed
```

- **[`docs/rules/README.md`](../rules/README.md)** — the front door: a short summary per topic, the
  derived glossary (the ubiquitous language), and the "Avoid these synonyms" list. Start here.
- **[`docs/rules/<topic>.md`](../rules/)** — the "now" story plus the rules for the area you're about
  to work in. There are six topics (execution-runtime, composition-operators, signal-time-dsp,
  authoring-library, agent-mcp, web-product-process).
- A rule's **rationale** (`docs/rules/rationale/<topic>/<rule>.md`) — open it only when you need the
  *why* behind a rule; its `Distilled from:` line is the sole surviving pointer to the ADR history.

[`docs/adr/`](../adr/README.md) is the **live iteration surface** — one file per decision that is
still moving. Once a decision solidifies, the `absorb-adrs` skill distills it into a rule + rationale
and deletes the ADR. Read a live ADR only for a decision that is still in flight; the settled design
is in the rules.

## Use the glossary's vocabulary

When your output names a domain concept (an issue title, a refactor proposal, a hypothesis,
a test name), use the term as defined in the [rules index glossary](../rules/README.md#glossary) —
Operator, Instrument, Rig, Plan, Swap, Voice, and so on. Don't drift to the synonyms the
[Avoid these synonyms](../rules/README.md#avoid-these-synonyms) list explicitly calls out (e.g.
"node", "module", "patch" as a noun), or to retired terms (e.g. "Lane").

If the concept you need isn't in the glossary yet, that's a signal — either you're inventing
language the project doesn't use (reconsider) or there's a real gap (note it for
`/domain-modeling`).

## Flag rule conflicts

If your output contradicts an existing rule, surface it explicitly rather than silently
overriding:

> _Contradicts the `osc-only-core` rule ([signal-time-dsp](../rules/signal-time-dsp.md)) — but worth
> reopening because…_

A settled rule that genuinely needs to change is reopened as a new **ADR** under
[`docs/adr/`](../adr/README.md) (the iteration surface), which a later `absorb-adrs` sweep folds back
into the rules.
