# Why: A serde type belongs to the contract if core produces it, and to a door's wire module if it exists only because that door exists.

[Rule](../../agent-mcp.md#contract-holds-what-core-produces)

Every [door](portable-tool-contracts.md) needs serde types, and the two candidate homes look
similar from inside any one door: a shared contract module, or the door's own wire module. Getting
the split wrong is quiet in both directions. A door-specific shape parked in the contract becomes a
type every other door must acknowledge and none of them means anything by. A genuinely shared type
duplicated per door drifts, and drift between doors is precisely what the
[one-source tool contract](portable-tool-contracts.md) exists to prevent.

The test is **provenance, not usage**: does core itself produce this? A swap report is returned by
the Coordinator's own swap, so it — and the report, diagnostic, diff-summary, and content-hash types
it is made of — is door-agnostic and lives in the contract, shared by every door. A door's wire
module owns its envelope (verbs, reply tags, framing) plus the payloads that exist *only because
that channel exists*. Stated as a question you can actually answer: **would this type still mean
anything with the channel deleted?** If no, it belongs to the channel.

`Conflict` is the worked example, and it is worth following because usage-based reasoning gets it
wrong. Three surfaces carry it — the channel's response, the client's outcome, the tool's field —
which makes it look shared. But core has no conflict type and no guard that could produce one: a
core swap is unguarded last-write-wins, and [the expect guard is a door
concern](expect-guard-is-a-door-concern.md). *This channel* decides that its clients get a guard and
that a miss is a distinct answer rather than a rejected report. Delete the channel and the type
means nothing. `DocumentSnapshot` is the same shape of answer: core exposes the document and the
installed hash separately, and pairing them is a wire decision.

The rule is about **payload types**, which leaves room for a deliberate exception on constants: the
channel's default address lives beside the wire types even though it is not a payload, because both
ends must agree on one literal and next to the types both ends serialize is where that agreement is
hardest to break. Naming it as an exception rather than stretching the rule to cover it keeps the
rule sharp.

Decided in: issue #639 — settled directly, no ADR.
