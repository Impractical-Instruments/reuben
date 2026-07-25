# Why: `send` is ephemeral live audition, clobbered at the next swap, and the document is the durable truth — so the authoring loop is try-then-commit.

[Rule](../../agent-mcp.md#try-then-commit)

The two paths carry deliberately different durability. **`send` is ephemeral audition** — sweeping a
cutoff, trying a tempo — living in render state only, so the next swap re-reads inputs from the
installed document and un-folded tweaks are **clobbered by design**. **Editing the document is for
keeping.** That gives the loop its natural shape, which the tool descriptions and the skills state
outright: send to explore, edit + swap to keep.

A `send`-survives-swap rule was rejected because render state would then win over document values,
and the sound would quietly drift from the file that is supposed to be true. The document is the save
source; anything that outranks it makes "what is playing" unanswerable from the file.

The split survived the move to incremental document verbs ([document-verbs](document-verbs.md)),
where folding `send` into the document became newly tempting — it would collapse two gestures into
one. It was **rejected on cost**: every auditioned value would then become validate + write + gapless
swap, making the cheap exploratory gesture exactly as expensive as the durable one, which is the whole
distinction being paid for. `send` therefore takes no `source` and stays live-only. If the authoring
harness ever shows small models thrashing on the split — "why did my change vanish?" — that is worth
revisiting as a *measurement*, not a guess.

Distilled from: ADR-0045, ADR-0066
