# Why: The window declares the types every consumer sees and their serialization, and the engine's equivalents stay internal — the API is not a projection of the engine's surface and owes it no mirror.

[Rule](../../agent-mcp.md#window-declares-its-own-types)

Every [door](portable-tool-contracts.md) needs serde types, and the reflex fix for having several
copies of them is to share the engine's. It works mechanically — a door field composed onto an
engine-owned args struct with `#[serde(flatten)]` produced a byte-identical advertised schema and
identical runtime deserialization. It settles for the wrong thing. Sharing leaves the engine's public
shape hostage to what the wire needs, and that is how the edit surface became nineteen flat
positional functions, one of them carrying nine positional parameters behind an
`#[allow(clippy::too_many_arguments)]`. The consumer's shape had been pushed into the engine.

**The point is decoupling, and it should be argued as decoupling.** With the wire declared at the
window, what is behind it is free to pick representations serde is bad at — interned strings, indices
instead of names, packed enums — which pays off in load and Swap. It is **not** a Render-path win:
nothing on the wire is hot, and document editing happens off the RT thread. Anyone citing this for
render performance is citing it wrong, and the render boundary is where the decision explicitly stops
([render-half-is-re-exported](../execution-runtime/render-half-is-re-exported.md)).

The measured alternative is what makes the case concrete rather than aesthetic. Hand-written per-door
argument structs against the engine's functions had already drifted three times while every test
stayed green: two wire renames, one struct missing a field whose body compensated with a literal
`&None`, and a dead `#[serde(rename)]` renaming a field to its own spelling. Nothing asserted the two
agreed, because nothing could — a copy is checked for arity at its call site and for nothing else.

**The document format is the exception, and it is not a small one.** The instrument document is not a
publicly-exposed type whose serialization is an API concern; it is a durable file format that must
round-trip byte-stably, and its loader is the [single validation
authority](loader-single-authority.md). It stays behind the window, which refers to a document by
`source` and never by value — which [no-resource-bytes](no-resource-bytes.md) already requires.

**The cost is a conversion layer**, roughly one mapping per verb plus the result types. It is real
code, and it is compiler-checked, which the duplication it replaces was not. What it cannot check is
*meaning*: the window and the internals can disagree about what a field means while both compile.
That residue is smaller than a duplicated list, and it is the price of the boundary — worth
remembering the first time a field is renamed at the window.

This retires an older discriminator. The question used to be provenance — *does the engine itself
produce this type?* — and that stops deciding anything once the window declares its own result types
regardless. The worry underneath it survives: a door-specific shape parked in a shared home is noise
for every other door. It is answered now by which side of the window a type is on, not by where it
was born.

Distilled from: ADR-0068
