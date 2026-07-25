# ADR-0068 — reuben-api declares its own types; core sheds public serialization

## Context

[ADR-0067](0067-core-splits-and-reuben-api-is-the-one-window.md) establishes `reuben-api` as the one
window. This decides what flows through it.

Today the split runs the other way. Core owns the wire types — `EditResult` derives `Serialize`
always and `JsonSchema` behind an optional `schemars` feature that exists solely so a door can
advertise an `outputSchema`. The *argument* surface is not shared at all: each door hand-writes one
params struct per verb. Issue #633 measured that surface and found nineteen core functions against
eighteen door structs, exactly one door-only field, zero tool bodies doing per-door work, one
expression repeated nineteen times, no guard asserting the two agree, and three drifts already
shipped — two wire renames, one struct missing a field its body compensates for with a literal
`&None`, and a dead `#[serde(rename)]` that renames a field to its own spelling. A third copy lives
in another repository.

The reflex fix is to share core's types with the doors. It works: `#[serde(flatten)]` composing a
door field onto a core-owned args struct produces a byte-identical advertised schema under the
pinned schemars and rmcp, with identical runtime deserialization. But sharing settles for the wrong
thing. It leaves core's public shape hostage to what the wire needs, which is how `edit::` ended up
as nineteen flat positional functions in the first place.

## Decision

**`reuben-api` declares its own types and their serialization.** Core's equivalents become internal.
The API is not a projection of core's surface and carries no obligation to mirror it: CRUD is flat
at the window because flat is what MCP and generated wrappers want, while the internals stay free to
be trait- and struct-shaped, and to condense several window calls into fewer internal ones.

The point is decoupling, and it should be argued as decoupling. Core becomes free to pick
representations serde is bad at — interned strings, indices instead of names, packed enums — which
pays off in load and Swap. It is **not** a Render-path win: nothing on the wire is hot, and document
editing happens off the RT thread. Anyone citing this decision for render performance is citing it
wrong.

**The document format stays in core, and core stays its sole validator.** The instrument document is
not a publicly-exposed type whose serialization is an API concern; it is a durable file format that
must round-trip byte-stably, and the loader is its one authority. The API refers to a document by
`source`, never by value — which the no-resource-bytes posture already requires.

**Model-facing prose lives in `reuben-api`.** Field descriptions ride the API's own types; the
tool-level sentence lives beside them. It is not core's, because core no longer owns the wire, and
it is not the door's, because a per-door copy is the duplication this decision exists to delete —
prose is the least mechanically-defended surface here and the one that has already rotted twice.

**The API surface gets no completeness tests. Consumers are the completeness test, and a test that
exists for its own sake gets deleted.** A window that is missing something is discovered by the
consumer that needed it, and then it is added.

## Consequences

**`agent-mcp.md#contract-holds-what-core-produces` is overturned.** Its test is provenance — *does
core itself produce this?* — and under this decision that question stops deciding anything: core
produces `EditResult` and the API declares its own result type regardless. The rule's underlying
worry stays real (a door-specific shape parked in a shared home is noise for every other door), but
provenance is no longer the discriminator.

**`agent-mcp.md#portable-tool-contracts` is overturned as written.** It puts the tool contract types
in `reuben-core` and has every door generate schemas from that one source. The one source becomes
`reuben-api`. The rule's intent survives exactly; its named location does not.

**#633 is subsumed rather than solved.** Its question was how to single-source the per-verb argument
surface across doors. Under this decision `reuben-mcp` has no params structs at all — it consumes
the API's types directly, so all nineteen are deleted rather than generated or shared. The flatten
measurement stands as a recorded fact and is no longer load-bearing: `expect` becomes an ordinary
field on a type we own.

**Two hand-maintained lists go.** `VERB_COVERAGE` — a table of every format leaf against the verb
that writes it — is a completeness test of the API surface maintained by hand, and its real consumer
test already ships: the agent-surface eval measures freehand JSON, which is precisely what an agent
produces when it lacks a verb. The MCP door's roster parity test goes with it; it exists because the
two lists were two, and it was the exemplar that motivated marking parity tests as defect markers at
all. Deleting the second list is the better end of that argument.

**Core sheds its optional `schemars` feature** and the `cfg_attr` that carries it, since nothing in
core is advertised any more.

**The cost is a conversion layer**: roughly one mapping per verb, plus result types. It is real code
and it is compiler-checked, which the copy it replaces was not — the old one was checked for arity
at a single call site and for nothing else. What it cannot check is meaning: the API and the
internals can still disagree about what a field *means* while both compile. That residue is smaller
than a duplicated list, and it is the price of the boundary.
