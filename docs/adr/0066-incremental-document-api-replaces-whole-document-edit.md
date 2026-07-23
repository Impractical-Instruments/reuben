# ADR-0066: The incremental document API replaces whole-document edit as the agent's contract

## Status

Accepted (2026-07-22). Decided through issue
[#583](https://github.com/Impractical-Instruments/reuben/issues/583), spun out of the
efficient-agent-authoring series ([#574](https://github.com/Impractical-Instruments/reuben/issues/574)).
Landing across children: the `write_text` resolver seam
([#601](https://github.com/Impractical-Instruments/reuben/issues/601)) is merged; the structural
projection ([#600](https://github.com/Impractical-Instruments/reuben/issues/600)), the closed verb
vocabulary and its CI guard ([#603](https://github.com/Impractical-Instruments/reuben/issues/603)),
and retiring the doc-in-context tool arms
([#604](https://github.com/Impractical-Instruments/reuben/issues/604)) are still in flight.

**This reverses [whole-document-edit](../rules/rationale/agent-mcp/whole-document-edit.md)**
(distilled from ADR-0045) and **extends
[no-resource-bytes](../rules/rationale/agent-mcp/no-resource-bytes.md)** (ADR-0049).

## Context

`#whole-document-edit` made the unit of a conversational edit the **whole `InstrumentDoc`**: the
model emits the full document, the loader validates it, the Coordinator swaps it. Its rationale was
emphatic that this is *"the committed contract, not a stopgap: no add-node/rewire tool surface exists
or is reserved,"* and it left exactly one door open:

> Incremental editing is not ruled out forever, but if it ever comes it is *sugar* over this
> contract — any command must resolve to apply-to-document → re-validate the whole document → swap.

The efficient-authoring series ([#574](https://github.com/Impractical-Instruments/reuben/issues/574))
measured what that contract costs a **desktop-class model**, the ones cheap enough to run locally.
The whole-document read is lossless *by obligation*: because the model is on the hook to re-emit
every byte it isn't changing, it must hold every byte it isn't changing. On `acid-techno.json` (53
nodes) the harness ([#598](https://github.com/Impractical-Instruments/reuben/issues/598)) put a
one-value tweak at **~2,098 re-emitted document characters** every turn — the expensive, error-prone
half of authoring is exactly the JSON the model never wanted to touch.

#583 walked through the "sugar" door and then knocked out the frame. The mechanical condition it
reserved is **satisfied** — every verb resolves to apply → re-validate the whole document through the
loader → write, so [loader-single-authority](../rules/rationale/agent-mcp/loader-single-authority.md)
is untouched. What is **not** satisfied is the word *sugar*: the incremental surface becomes
**primary and exclusive**. The agent has no whole-document path at all, because under the extended
resource posture it may not read or write document bytes.

## Decision

**The agent authors through a closed vocabulary of path-addressed, stateless, engine-free document
verbs, and never loads a reuben-owned document into its context.** `#whole-document-edit` is
overturned; the whole-document read/emit obligation is gone.

### 1. `#whole-document-edit` is overturned — its two objections answered

Its rationale rejected an incremental surface on two grounds; both are now answered rather than
waved away:

- **Per-command validation semantics** (a second authority that would drift from the loader). There
  are none. Every verb applies to the document and re-validates the **whole** document through the
  engine's own load-plus-instantiate path — the same single authority, unmoved. A verb is a way to
  *produce the next document*, not a way to check one.
- **A command vocabulary invented ahead of need.** The vocabulary is not invented — it is **derived
  from the document format**, which is already the spec, and a CI completeness guard walks the format
  types and fails the build when a field no verb can reach is added (the same spirit as the
  native-vs-wasm `describe` parity guard). See the vocabulary ticket
  ([#603](https://github.com/Impractical-Instruments/reuben/issues/603)).

### 2. `#no-resource-bytes` is extended, not reversed

Its principle — *reuben-owned bytes don't ride the agent's context* — now covers **instrument JSON**,
which the rule never addressed only because documents were assumed to be the model's own output. The
**sample gesture is unchanged**: `cp foo.wav` next to the instrument still loads no bytes into
context, and the rule's stated revival condition — the persona changing to a packaged, non-dev client
— is *not* what happened here. A local model authoring by verb is the same dev persona; what changed
is that the document joined the sample as "a resource the agent references, never carries." That was
already half-true — voices and subpatches are documents referenced as resources.

Two clauses in that rationale stop being true and are retired:

- **The "no server-side write path exists" symmetry clause.** A server-side document write path now
  exists by design (§4).
- **The MCP sidecar's "filesystem writes are the agent's own gesture" stance.** The sidecar writes
  documents now; the write is the door's, not the agent's file tools'.

### 3. The read side is a lossy projection, and that is the point

The agent's whole view of a document becomes a **structural projection**, single-sourced in
`reuben-core`: a **node index**, a **node zoom** (a node's inputs plus its inbound sources *and
outbound consumers* — reverse edges), and a **pipe index/zoom** (interface pipes need their own
split — `acid-techno.json` carries 90 of them in a 5,985-byte `interface` block).

The justification is **losslessness dropped, not compression bought.** A 1:1 structural projection of
`acid-techno.json` is 4,483 B against the 35,462 B file — only ~3×, pure encoding, and not worth an
API on its own. The real win is that removing the re-emission obligation makes a **partial** read
legal: index + zoom of the two nodes a turn touches is **~1.7 KB against 35 KB, ~20×**, and the ratio
grows with document size (the index grows linearly, each zoom stays flat). Compression would have
preserved every byte; this preserves only the bytes the turn needs.

The design is filed as its own **grilling** ticket
([#600](https://github.com/Impractical-Instruments/reuben/issues/600)), not a task, because the
projection becomes the agent's entire model of the document *permanently* and **its omissions are
silent** — unlike JSON, which is lossless by construction. The reverse-edges requirement was caught
by accident while discussing `remove_node`; that is weak evidence more gaps hide, so the series
harness is pointed at the projection specifically.

### 4. Documents are door-resolved resources, not filesystem paths

A verb's `source` is **opaque and door-resolved**, exactly as
[portable-tool-contracts](../rules/rationale/agent-mcp/portable-tool-contracts.md) requires.
`ResourceResolver::resolve_text` already did the read half door-abstractly (it is how a nested voice
patch loads today); the write half joins it as `write_text(source, text)`
([#601](https://github.com/Impractical-Instruments/reuben/issues/601), merged). `FsResolver` writes
the file; the browser's memory resolver writes the host store. So the verb means **one thing behind
every door** and the web door needs no filesystem. Consequence: `ResourceResolver` stops being
read-only, and the MCP sidecar formally becomes a process that writes to disk.

### 5. Write safety, and why removal is the exception

`add_node(source, address, type) -> {report, hash, zoom}` is the shape: opaque `source`, a post-write
hash **always** returned, and every mutate verb echoes the zoom of what it touched.

- **Write-iff-valid, no transactions.** `LoadError` has no unwired-input or unreachable-node error,
  so `new_doc → add_node → add_node → wire → wire` is valid at *every* intermediate step — the build
  need not be atomic.
- **`remove_node` cascades and reports the breakage.** Removal is the one edit that can invalidate:
  deleting `/clock` from `acid-techno.json` leaves 5 dangling wires (`LoadError::UnknownNode`,
  fatal). So `remove_node` auto-unwires every consumer and reports what it broke, matching how a
  dissolved subpatch already drops touching wires and announces a dark-degrade warning. Refusing
  would turn the commonest structural edit into a multi-call discovery exercise.
- **`expect` is optional.** Per
  [expect-guard-is-a-door-concern](../rules/rationale/agent-mcp/expect-guard-is-a-door-concern.md),
  core's `write_text` stays unguarded last-write-wins and the door does the content-hash compare;
  documents inherit the posture that already exists. Mandatory `expect` was rejected — it forces a
  read before every write and doubles the call count. The clobber window in fact **shrinks**:
  whole-document edit held the file across an entire turn; per-call read-modify-write holds it for one
  call.

### 6. `send` is untouched

`send` stays **live-only and ephemeral** (no `source` argument), clobbered at the next swap, so
try-then-commit survives. Folding it into the document was considered and **rejected on cost**: every
auditioned value would become validate + write + gapless swap, making the cheap audition gesture as
expensive as the durable one. *If the series harness later shows small models thrashing on the split
("why did my change vanish?"), this is worth revisiting — as a measurement, not a guess.*

## Alternatives rejected

- **A handle-addressed `docID` workspace.** Needs a session; core is stateless and the CLI is a cold
  process per invocation, so there is no place to hold a workspace —
  `#expect-guard-is-a-door-concern`'s door/core split applies. (This also kills the in-memory
  workspace floated in [#585](https://github.com/Impractical-Instruments/reuben/issues/585).)
- **The value-addressed `(document, …) -> {document, …}` form.** The document rides the context
  **both ways**; the harness ([#592](https://github.com/Impractical-Instruments/reuben/issues/592))
  pins metric (c) to count echoes, so this scores *worse* than a plain re-emit — the opposite of the
  goal.
- **A `replace_document(source, json)` escape hatch.** Every vocabulary gap would quietly route
  through it and the API would never get finished. Closing the hatch is what makes completeness a
  correctness requirement the CI guard can enforce.

## Consequences

**The thing being ratified is the reversal itself** — that the incremental surface is primary and
exclusive, not sugar — not the individual verbs, which land through their own children (#600, #603,
#604). The measurement that justified it: a realistic one-value turn drops from a 35,462 B document
obligation to ~1.7 KB of index + zoom, ~20×, because losslessness is no longer required — not because
anything was compressed.

**A hard constraint on [#577](https://github.com/Impractical-Instruments/reuben/issues/577):** its
proposed generator→output reachability check **must stay a warning**. As an error, every intermediate
build step (`add_node` before the matching `wire`) would fail write-iff-valid and incremental
authoring would become inexpressible.

The read/write asymmetry with `#no-resource-bytes` is now uniform: on every lane — native, MCP,
web — no reuben-owned bytes ride the agent's context, the agent references by opaque source, and the
host (the door's resolver) moves the bytes. Documents simply joined samples under that posture.
