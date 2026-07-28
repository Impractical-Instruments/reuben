# Why: Every argument slot a verb advertises declares the value forms it accepts — the other half of the closed surface, which governs only the keys — because a client that coerces a call against the advertised schema has nothing to coerce an undeclared slot to, and sends a number as a string.

[Rule](../../agent-mcp.md#typed-argument-surface)

The [closed argument surface](closed-argument-surface.md) settled the key side: an argument the
window does not declare is a refusal. The value side was left open by accident. A slot typed
`serde_json::Value` renders as a schema with no `type` keyword at all, and a client that coerces its
arguments against the advertised schema — the one on the other end of the sidecar does — then has
nothing to hold the argument to. `value: 110` arrives as `"110"`, the loader reads a symbol where a
number belongs, and the edit is correctly refused. Every numeric point-edit in the roster was dead
this way, while the whole test suite stayed green: the tests build request JSON by hand, so they
exercise a door no user reaches.

The asymmetry is what made it hard to see. Only *top-level properties* were being coerced, so the
map-shaped arguments on the one-shot add kept working — they declared `type: object`, and values
nested inside arrived intact. That is not a safe place to be either; it is the same defect one level
down, waiting for a client that coerces deeper.

**The field type stays raw JSON, and only the advertised schema changes.** Retyping the field would
undo two decisions that are load-bearing:

- **The verb owns its refusal.** A literal slot refuses a wire-ref object by name and points at
  `wire_instrument_input`; a control argument out of `f32` range is refused with the number in the
  message. Those are hand-written precisely so a caller learns what to do next. Behind serde they
  collapse into "did not match any variant", which a model cannot act on.
- **The integer/float split is JSON's own.** A control atom rides untagged, so a JSON integer becomes
  an `I32` and a float an `F32` with no rule for a client to reimplement. A single advertised numeric
  type is right for the wire; a deserializing enum in the field would have to re-decide it.

So the two sets — advertised and accepted — are maintained as a pair rather than derived from one
another, and the advertised set is deliberately *narrower* in one place: the wire-ref object is
absent from the literal slots, because those verbs reject it. Advertising a form the verb refuses
would invite the call.

The guard reads the wire, not the declarations, for the reason the
[prose guard](../code-as-grounding/wire-descriptions-are-model-facing.md) does: a check on the arg
structs would catch `serde_json::Value` specifically and miss every other way a slot renders
typeless. Two details decide whether it is worth having at all. It must **resolve `$defs`** — the
batch verb's control arguments sit one `$ref` hop down, and a walker that stops at the reference
reports three offenders instead of six and goes green the day the top-level slots are fixed, with the
real one still broken. And it must **not be able to walk nothing**: a planted schema proves it still
finds a typeless leaf through a hop, an item schema and a map value schema, and a floor on the number
of slots visited catches a rename that would otherwise empty the walk into a vacuous pass. A guard
that cannot fail is worse than none, because it is believed.

Guarded by: crates/reuben-mcp/tests/stdio_tools_list.rs::every_advertised_property_constrains_its_value

Decided in: issue #692, #694 — settled directly, no ADR.
