# Why: `pipe` is a reserved operator `type_name`, refused by the one contract validator.

[Rule](../../composition-operators.md#pipe-is-a-reserved-type-name)

[Interface pipes](interface-pipes.md) are **loader-built**. An author declares them as
`interface.inputs` / `interface.outputs` entries, and the loader synthesizes the pipe nodes; there
is no registered `pipe` operator anywhere, and the descriptor a pipe node carries is built rather
than looked up. The save path relies on this from the other direction — it identifies pipe nodes by
that type name when writing the document back out.

That makes the name load-bearing in a way no other `type_name` is. A scaffolded operator called
`pipe`, or an embedder-registered descriptor with that name, makes pipe nodes and that operator
indistinguishable by the one field meant to tell them apart — and the save path starts writing an
operator node out as an interface entry, so the document round-trips into something else.

So the name is refused, and refused in the **contract validator** specifically. That is the one
place the macro and the scaffold both pass through, so a hand-written `operator_contract!` and a
`reuben scaffold-operator` invocation hit the same rejection, and the scaffold fails *before* it
generates any code rather than leaving a half-built operator to clean up. The registry carries the
same reservation for embedders that register descriptors directly, since they never touch the macro.

Decided in: issue #639 — settled directly, no ADR.
