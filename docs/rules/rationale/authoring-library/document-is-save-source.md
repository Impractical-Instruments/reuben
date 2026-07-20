# Why: The loaded and edited InstrumentDoc is the save source of truth, while from_graph is the one-way flatten and export path that inlines every nested reference.

[Rule](../../authoring-library.md#document-is-save-source)

Nesting inlines: building a graph **dissolves** each `subpatch` node into its flattened equivalent,
so `from_graph` of a built graph emits the inlined nodes and the reference is gone. That is exactly
right for export and exactly wrong if `from_graph` is read as "the save path" — it would silently
erase every nested reference on save.

So the two directions are named separately. **Saving means serializing the `InstrumentDoc` you
loaded and edited** — nested references survive verbatim via serde, untouched. `from_graph` is
deliberately the **flatten/export** path: every spliced subpatch appears as its inlined nodes, no
splice provenance is recorded on the `Graph`, and the render engine carries **zero authoring state**.
Editing flows mutate the document; they never reverse-engineer a built graph.

The forces that make this the right call are both correctness and future cost. Recording splice
provenance on the `Graph` (to re-fold subpatches on save) was rejected because it adds authoring
state the engine never renders with, and — worse — it makes *every* future build transform (metadata
overrides, namespacing, boundary rewires) carry an inverse forever. The document already holds the
truth, so the build pipeline is free to be a one-way, lossy-by-design flatten. This is the same
discipline the version gate depends on: because the document (not the built graph) is authoritative,
`from_graph`'s output can be routed back through the normalize gate as a current-shaped document
without any invertibility burden ([normalized-doc-gate](normalized-doc-gate.md)).

Distilled from: ADR-0036
