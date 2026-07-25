# Why: A grounding projection's size is gated relative to the full view it compresses, never as a flat per-item cap.

[Rule](../../agent-mcp.md#grounding-budget-is-relative)

The compact operator listing is what an agent reads to know what it can patch with, and its size is
a real constraint: it competes for context with the instrument, the conversation, and the rest of
[the grounding](grounding-single-source.md). A budget on it is right.

The wrong way to enforce that budget is a fixed character ceiling, because it makes the wrong thing
the variable. A flat ceiling divided by the current registry is a per-operator tax — and a tax
whose bill grows with the operator count fails after N more operators *regardless of whether those
operators carry any new information*. Adding a number operator that is one line different from its
neighbors and adds almost nothing to the listing counts exactly as much against the ceiling as
adding a genuinely novel one. The gate ends up blocking library growth that is both a
developer-experience win and, for the number-operator family, a measured render-thread improvement —
while doing nothing about the actual problem, which is that the projection is not compressing hard
enough.

So the lever is the **projection**: progressive disclosure of the library, family-aware
summarization, whatever makes a listing convey the same information in less. Stating that as a rule
matters because the flat cap is the path of least resistance every time the budget is next hit, and
it always looks like enforcing discipline rather than what it is — capping the library to protect a
projection that was never asked to improve.

The enforceable form of the budget is therefore **relative**: the compact listing must stay under a
fixed fraction of the full minified view it compresses. That scales with the registry by
construction — adding operators grows both sides — so it keeps measuring the thing the name promises
(is this actually compact?) and stays silent about how many operators exist. An absolute token target
still exists as the design goal; it is the *gate* that is relative, because a gate that fails for
reasons unrelated to the property it guards gets disabled, and a disabled gate watches nothing.

Decided in: issue #639 — settled directly, no ADR.
