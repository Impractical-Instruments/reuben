# Why: A door serves the rendered projection rather than a second encoding of it, and a view stays structured only where a program parses it.

[Rule](../../agent-mcp.md#one-serialization-per-view)

The [projections](document-projection.md) exist to produce a compact line grammar — that rendering
*is* the deliverable, not a convenience over some truer structured form. So a door that also emits its
own per-view JSON is carrying a second encoding of the same read, and a second encoding only one door
has is free to drift from the one every other consumer reads. Nothing asserts two serializations of
the same view agree, because nothing can: they are two answers to one question with no shared
statement of what the answer is.

The narrowing that followed was taken deliberately and checked first — every consumer of the CLI's
structured view output was enumerated, and nothing parsed it. A surface with no consumers is the
cheapest possible time to remove one.

**The exception is a real program, not a hedge.** The boundary view keeps its structure because the
control-surface generator parses those pipes: it needs fields, not a line to show a reader. That is
the test the rule turns on — a view stays structured where something *parses* it, and renders where
something *reads* it. Same question, two consumers, one verb underneath, and the boundary view is the
one whose shape is under test.

Distilled from: ADR-0070
