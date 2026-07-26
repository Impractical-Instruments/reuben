# Why: The resource seam is the one call in and the host implements it: the trait is the contract, the filesystem implementation this repo ships is one implementation behind a default-off feature, and the seam sits above both halves of the window because both call it.

[Rule](../../authoring-library.md#resource-seam-is-host-implemented)

Samples and nested documents are never handed to the engine as bytes
([no-resource-bytes](../agent-mcp/no-resource-bytes.md)), so the engine has to call back *out* through
a seam the host provides — the filesystem natively, the staging seam in the browser. It is the one
call in, and the window names it as a different kind of thing from the verbs a host calls, because a
wrapper generator has to treat "what you call" and "what you must provide" differently.

**The trait is the contract; the implementation is not.** A filesystem resolver ships here to be shared
rather than reimplemented, and it ships **off by default** — that is the whole point rather than a
packaging detail. A resolver that arrives switched on is one a host *inherits* rather than chooses, and
the resource seam is precisely where a host's authority over its own sources has to be explicit. A host
whose sources are not files writes its own and never links this one. It is not a separate crate,
because a crate for one struct is overhead with no boundary behind it.

**It sits above both halves of the window because both call it.** A document verb reads and writes
through the seam, and so does the load that builds the initial Engine a host renders. Parking it in the
authoring half would have meant a render-only host either compiling the authoring surface or meeting a
second copy of the trait — and a second implementation of the same four methods is the class of bug
where two answers to one question differ. There is one implementation.

Distilled from: ADR-0069, ADR-0072
