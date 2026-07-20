# Why: A conversational edit is the whole instrument document in and a report out — with no incremental edit-command surface — where send is ephemeral audition and the document is the durable truth (try-then-commit).

[Rule](../../agent-mcp.md#whole-document-edit)

The unit of a conversational edit is the **whole `InstrumentDoc`**: the model emits the full
document, the loader validates it, the Coordinator swaps it. Incremental edit commands (add-node,
rewire, retune) as the *contract* were rejected — they need per-command validation semantics (a
second authority for rules the loader already enforces, exactly the drift
[loader-single-authority](loader-single-authority.md) forbids), a command vocabulary invented ahead
of need, and they buy nothing a model can't already do by editing in its own context and re-emitting.
This is the committed contract, not a stopgap: no add-node/rewire tool surface exists or is reserved.
Incremental editing is not ruled out forever, but if it ever comes it is *sugar* over this contract —
any command must resolve to apply-to-document → re-validate the whole document → swap, keeping the
loader the single authority and the document the save source of truth. Scale does not bend the
contract: re-emitting a document never re-emits its subpatch references, so the remedy for a document
that outgrows comfortable re-emission is to factor it into subpatches — better authoring anyway — not
a different edit contract.

The two paths carry defined durability. **`send` is ephemeral audition** — sweeping a cutoff, trying
a tempo — living in render state only, so the next swap re-reads inputs from the installed document
and un-folded tweaks are **clobbered by design**. **Document edit + swap is for keeping.** This gives
the loop a natural **try-then-commit** shape the tool descriptions and skills state explicitly: send
to explore, doc-edit + swap to keep. A `send`-survives-swap rule was rejected because render state
would then win over document values and the sound would quietly drift from the file that is supposed
to be true. (This ADR also first pinned swap-survivor identity to address+type; that key was later
sharpened and now lives with the Swap machinery in the execution-runtime topic — only the edit
contract belongs here.)

Distilled from: ADR-0045
