# Why: The window's surface carries no completeness test of its own: the consumer that needed a verb is the report that it is missing, and the authoring eval's freehand-JSON count is the measurement that stands in for one.

[Rule](../../agent-mcp.md#consumers-are-the-completeness-test)

A window that is missing something is discovered by the consumer that needed it, and then it is added.
A test asserting the window is *complete* has to define completeness against something, and the only
thing available is a hand-maintained list of what ought to be there — which is a second surface,
maintained by hand, that can be right about itself and wrong about what anyone wanted.

The worked example is the one that was deleted. A coverage table dispositioned every leaf field of the
instrument format into the verb that writes it, and a guard walked the real format types and set-diffed
the two. It was green, it read authoritative, and its real consumer test already shipped: the authoring
eval counts **freehand JSON**, which is exactly what an agent emits when no verb reaches what it wants.
A missing verb shows up there as a number that moves, which is a report about authoring rather than
about a table.

**The asymmetry that keeps this from over-reaching**: the *read* surface's field coverage stays
guarded. A read surface can omit a field silently and nothing downstream notices — there is no
consumer to file the report, because a projection that quietly drops a field still parses. That
enumeration is also the one place a derived schema earns its keep, since the alternative to walking it
is a second hand-written list of format fields.

**What replaces a deleted guard is not always another guard**, and that is the decision rather than an
oversight. Of the three checks this posture removed, one is answered by a measurement, one by a
refusal to construct — the roster parity assertion moved down a layer into the stamping step, which
already walks the built router against the roster in both directions and refuses to start on a
mismatch — and one had no substitute, which is why the mechanism it needs survives. A parity marker is
a bet that two lists cannot be generated from each other
([parity-test-is-a-defect-marker](../code-as-grounding/parity-test-is-a-defect-marker.md)); this is
what it looks like when the bet comes due.

Distilled from: ADR-0068, ADR-0073
