# Why: The engine's own load-plus-instantiate path is the single validation authority, and validate runs exactly it — there is no second schema-validation gate.

[Rule](../../agent-mcp.md#loader-single-authority)

validate is defined as "does the engine's own load + plan path accept this?" — the loader is the
**single source of truth**. A second authority (a JSON-Schema validation pass, a per-edit command
validator) would be a rival that drifts from the rules the loader already enforces, giving two
answers to one question. So there is deliberately none: the schema, when it existed, stayed an
authoring aid an agent *reads*, never a gate — and it was ultimately dropped from grounding entirely
([grounding-not-schema](grounding-not-schema.md)).

Concretely, `ok` ⟺ the load succeeds *and* instantiate finds no cycle; errors carry the loader's
human message verbatim plus the node/port it already localized, lifted into structured fields so an
agent jumps straight to the offending node; warnings are advisory and never flip `ok` (an
unresolved sample plays silence — still valid). Every verb resolves to apply-to-document →
re-validate the whole document → write, so the loader stays the one authority
([document-verbs](document-verbs.md), [grounding-single-source](grounding-single-source.md)).

Distilled from: ADR-0020, ADR-0045
