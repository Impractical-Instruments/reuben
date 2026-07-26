# Why: A type-agnostic `arg` pass-through is opaque-payload-only, and what may wire into it is keyed on the source type's registered external OSC form.

[Rule](../../composition-operators.md#arg-passthrough-is-capability-keyed)

Almost every port commits to a vocab type, which is what makes [the per-wire form
check](per-wire-form-check.md) able to reject a bad patch at plan time. The boundary sink is the one
place that commitment is wrong: `osc_out` exists to put *whatever the patch produced* on the wire,
and enumerating the types it accepts would mean editing the sink every time the vocabulary grows. So
`arg` is a port type that commits to nothing, classified as an Event stream so routing delivers the
raw `Arg` unlatched and uncoerced, with the type-driven expansion happening at the boundary instead.

**Where it may be declared** is restricted to operators that treat the payload as opaque — a pure
carrier. The contract validator closes `arg` outputs and constants, leaving it input-only. The
reason is that the wired *source* port stays the type authority: an `arg` input asserts "I will not
look at this," and an operator that inspected the payload would be doing type dispatch that the
graph cannot see and the planner cannot check.

**What may wire into it** is where the obvious answer is wrong.
"Any Message-domain source" would admit types that have no external representation — `Harmony`
registers no OSC form — producing a wire that validates cleanly and can never send anything. That is
worse than a rejected patch: it is a patch that looks connected and is silently inert. So legality
is **capability-keyed** rather than kind-keyed: any Event or Value source whose type has a
registered external OSC form wires in, and a form-less type is a hard error. For a struct vocab type
that capability is exactly a converter registered through `register_osc_form!`, which makes "can this
cross the wire" a property a type opts into once rather than a list two checkers maintain separately.

Signal sources are rejected by the same statement, and that is the second thing the rule buys:
**audio stays off the wire by construction**, not by a special case someone has to remember. A dense
per-sample buffer has no sane OSC form, so it has no registered form, so it cannot wire — the
general mechanism produces the specific guarantee.

The load-time check and the planner's per-wire check both call the same capability predicate. That
is deliberate: two independent spellings of "which types may cross" is exactly the drift that lets a
document load and then fail to plan, or worse, plan and stay silent.

Decided in: issue #639 — settled directly, no ADR.
