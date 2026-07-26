# Why: A grounding projection's size is gated relative to the full view it compresses, never as a flat per-item cap — not built: the only gate is an `#[ignore]`d test, so nothing currently watches the listing's size.

[Rule](../../agent-mcp.md#grounding-budget-is-relative)

The compact operator listing is what an agent reads to know what it can patch with, and its size is
a real constraint: it competes for context with the instrument, the conversation, and the rest of
[the grounding](grounding-single-source.md). A budget on it is right.

A fixed character ceiling makes the wrong thing the variable: divided by the current registry it is
a per-operator tax, so it fails after N more operators regardless of whether those operators carry
any new information — blocking library growth while doing nothing about the actual problem, which is
that the projection is not compressing hard enough. So the lever is the **projection**: progressive
disclosure, family-aware summarization, whatever conveys the same information in less.

The enforceable form of the budget is **relative** (designed, not yet live — the gate ships inside an
ignored test): the compact listing must stay under a
fixed fraction of the full minified view it compresses. That scales with the registry by
construction — adding operators grows both sides — so it keeps measuring the thing the name promises
(is this actually compact?) and stays silent about how many operators exist. An absolute token target
still exists as the design goal; it is the *gate* that is relative, because a gate that fails for
reasons unrelated to the property it guards gets disabled, and a disabled gate watches nothing.

Decided in: issue #639 — settled directly, no ADR.
