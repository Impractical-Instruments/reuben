# ADR-0073 — The hand-maintained lists go; core keeps `schemars` for the format guard alone

## Context

[ADR-0068](0068-reuben-api-declares-its-own-types.md) decided what flows through the window and, in
its consequences, named three things that go once nothing in core is advertised any more:
`VERB_COVERAGE`, the MCP door's roster parity test, and core's optional `schemars` feature with the
`cfg_attr` that carries it. The first four phases of the window built the surface that makes those
true. This is the pass that collects them, and one of the three does not survive contact.

`VERB_COVERAGE` and the roster parity test are both lists held level by an assertion. The parity test
is the one that motivated
[`code-as-grounding.md`](../rules/code-as-grounding.md)'s marker convention in the first place, so
its disposition is the interesting half: a marker is a bet that the two lists cannot be generated
from each other, and this is the bet coming due.

`schemars` is where the ADR's consequence was written before anyone counted the derives.

## Decision

**`VERB_COVERAGE` and its guard are deleted.** The table dispositioned every leaf field of the
instrument format into the verb that writes it, and the guard walked the real format types and
set-diffed the two. It was a completeness test of the window's own surface, maintained by hand,
against the surface ADR-0068 says gets none: the consumer that needed a verb is the test, and the
authoring eval already counts freehand JSON — which is exactly what an agent emits when no verb
reaches what it wants. The projection's `FIELD_COVERAGE` is untouched and stays: a *read* surface can
omit silently, and nothing downstream notices.

**The roster parity assertion moved into construction rather than being dropped.** `stamp_window_prose`
already walked the built router against `reuben_api::tools::CONTRACTS` in both directions and refused
to start on a mismatch — it had to, because an unstamped route falls back to rmcp's rustdoc and hands
Rust-reader prose to a model. That is the same claim `advertises_the_declared_roster_over_stdio` was
observing after the fact, so the test and its in-crate twin `the_declared_roster_is_registered` are
deleted. What the stdio test keeps is what only the wire can answer: every roster verb carries an
`outputSchema`, every one advertises the window's own sentence, and no advertised description carries
markup a model cannot resolve.

This is the marker being **cashed**, and it is worth naming as the outcome the convention exists to
make available. The recorded reason was true as written — the advertised list is produced by another
process and can only be observed — and it was still the wrong thing to keep, because the property it
observed was available one layer down as a refusal to start.

**Core keeps its `schemars` feature, narrowed to `src/format` and to one guard.** ADR-0068's
"core sheds its optional `schemars` feature" holds for every type a door reads — the report types in
`contract.rs`, `EditResult`, the `introspect` views, and all fourteen projection view types shed
their derives here, along with the two tests that asserted those schemas' shapes. It does not hold
for the format types. `FIELD_COVERAGE`'s guard enumerates the instrument format's leaf fields by
walking `schema_for!(InstrumentDoc)`, and that enumeration is the whole point: the alternative to a
derive is a second hand-written list of format fields, which is the thing this pass is deleting.

So the feature survives with exactly one consumer, no dependent enabling it, and CI naming it on the
test step. That is unusual enough to be worth stating rather than leaving for someone to discover as
a dormant feature and remove.

## Consequences

**One claim in the corpus changed from mechanical to measured.**
[`rationale/agent-mcp/document-verbs.md`](../rules/rationale/agent-mcp/document-verbs.md) argued that
the verb vocabulary is derived rather than invented, and cited the CI guard as proof. The argument
survives; its proof is now the eval's freehand-JSON count. The rule sentence is untouched — what
changed is the mechanism the rationale named, and a rationale naming a guard that no longer runs is
the failure mode the rules corpus was swept for.

**The advertised surface did not move**: 27 tools, 72,976 bytes, eval gate green at 17,389 fixed
grounding tokens. Nothing here was ever on a wire.

**What replaces a deleted guard is not always another guard.** Two of the three deletions here leave
no mechanical check behind, and that is the decision rather than an oversight: one is answered by a
measurement (the eval), one by a refusal to construct. The third — the format's field enumeration —
is the case where no substitute exists, which is why the feature it needs stays.

**#633 is closed by this**, having been subsumed by ADR-0068 rather than solved: the door has no
params structs at all, so there is no second argument surface left to single-source.
