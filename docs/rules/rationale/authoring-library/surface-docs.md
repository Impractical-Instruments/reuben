# Why: Player-facing controls are the instrument's interface input pipes, and presentation lives in a separate reference-based surface doc that binds pipe names to widgets.

[Rule](../../authoring-library.md#surface-docs)

Two playable-surface mechanisms had accreted — inline per-node `control` blocks and the named
`interface` input pipes — read by two resolvers in two languages that drifted, both
reverse-engineering ranges from `map` instance literals and sniffing sequencer gate steps because a
`control` block carried no contract of its own. `NodeDoc.control` was an **opaque passthrough the
engine never read**, so retiring it was render-safe by construction.

The resolution picks **one boundary**: every player-facing control is an `interface` **input pipe**
— a named entry with a declared type, engine-enforced against every consumer wire (the pipe/device
layer lives in [composition-operators](../../composition-operators.md); this rule only builds the surface
*on top of* it). A surface references pipes **by name**; the instrument's `interface` block *is* the
contract, so a control sends OSC to the pipe's minted `/<name>/in` address that `describe` already
uses. Pipe types cover the whole widget set — `f32`/`f32_buffer` back faders and toggles, `note`
pipes back note-toggles and chord-buttons.

The split line is deliberate: the **pipe carries the quantity** (`type`/`min`/`max`/`default`/`unit`/
`curve`), so every surface of that instrument inherits them; the **surface carries the presentation**
(`bind`, `label`, `widget`, `group`, order, and an optional *narrower* range a resolver clamps to
the pipe range). `unit` and `curve` were considered for the surface and rejected — they describe the
quantity, not one rendering of it, and two surfaces of the same instrument must not disagree on what
Hz means. Presentation lives in a **separate reference-based surface doc** that stores the pipe
*name* and merges the pipe's inline contract at load, so changing a pipe's range makes every surface
follow — drift-free by construction. This is why the duplication dissolves: "resolve a control"
collapses to *read the pipe, merge the surface overrides, pick a widget*, and the range-archaeology
and gate-step sniffing in both resolvers go away. A sequencer's N gate steps become N ordinary pipes
(`kick_step1..16`), each defaulting to the old inline literal so the rest state is unchanged — no
new lane/indexed-pipe machinery, just the honest, discoverable, engine-validated place.

Distilled from: ADR-0043, ADR-0018, ADR-0017
