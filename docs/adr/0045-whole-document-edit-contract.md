# ADR-0045: Whole-document edit contract; Swap survivors match on address + type

## Status

Accepted (2026-07-11). The edit-granularity decision of the reuben MCP server effort —
wayfinder ticket [MCP/C (#272)](https://github.com/Impractical-Instruments/reuben/issues/272)
on map [#270](https://github.com/Impractical-Instruments/reuben/issues/270), deciding open
question 2 of [#220](https://github.com/Impractical-Instruments/reuben/issues/220). **Rides on**
[ADR-0009](0009-graph-lifecycle.md) (Swap migrates survivors "matched by stable identity" —
this ADR pins the identity), [ADR-0012](0012-boundary-and-threading.md) (control vs structure
split; the Coordinator command queue), [ADR-0020](0020-introspection-and-patcher-skill.md)
(the loader is the single validation authority), [ADR-0036](0036-instrument-library-and-format-versioning.md)
(the document is the save source of truth), and [ADR-0044](0044-mcp-stdio-sidecar.md)
(stdio sidecar, user-owned engine); **amends none**. Feeds the Coordinator/Swap design (MCP/D)
and the MCP tool surface (MCP/E).

## Context

Conversational instrument authoring needs a unit of edit: the model emits a **whole
`InstrumentDoc`** that gets validated and swapped, or **incremental edit commands**
(add-node / rewire / retune). Facts that bore on it:

- The document is already the save source of truth: editing flows mutate the document, never
  reverse-engineer a built graph (ADR-0036 §1). Nested references survive in the doc via serde,
  untouched — a document *references* its subpatches, it does not contain them.
- `validate` is defined as "does the engine's own load + plan path accept this?" — whole-document
  granularity only. There is no per-edit validation semantics anywhere, and ADR-0020 explicitly
  rejected a second, drifting validation authority.
- Every node carries a unique `address` (duplicate = fatal `LoadError`), and inlined subpatch
  nodes take their host node's address as a prefix — the built graph has a total, stable,
  human-meaningful naming scheme already.
- ADR-0009's Swap promises to "migrate survivors' state, matched by stable identity" — the
  vocabulary existed; the matching key had never been chosen.
- ADR-0012 separates **control** (message queue Render drains; no Swap) from **structure**
  (command queue to the Coordinator). A whole-document install is itself a command, so the
  queue does not presuppose an incremental vocabulary.
- Real instruments in-repo are 0.8–20KB of JSON (≈5K tokens at the top end); models are good at
  re-emitting full JSON documents and already do their "incremental" reasoning in their own
  context.

## Decision

### 1. Whole-document is the committed edit contract — M1 and M2, not a stopgap

The unit of a conversational edit is the **whole `InstrumentDoc`**: the model emits the full
document, the loader validates it (single authority, ADR-0020), the Coordinator instantiates
and swaps it (ADR-0009). This is the contract the epic builds against: MCP/D designs the
Coordinator channel around "install this document", and MCP/E's tool contracts reserve no
edit-command surface.

**Considered and rejected:** *incremental commands as the contract* — needs per-command
validation semantics (a second authority for the rules the loader already enforces, exactly
ADR-0020's rejected drift), a command vocabulary invented ahead of need, and buys nothing the
model can't already do by editing in-context and re-emitting; *whole-document as stopgap* —
leaves MCP/D and MCP/E designing against a contract expected to dissolve, reserving surface
for a successor that may never come.

### 2. Swap survivor identity: fully-qualified node address + operator type

At Swap, a node in the new Plan is a **survivor** — keeps its state (ADR-0009) — iff a node
with the **same fully-qualified address and the same operator type** exists in the old Plan.
Matching happens over built graphs, where subpatch splice has already prefixed every nested
address, so nesting needs no special case.

Consequences, accepted and documented rather than mitigated:

- **Renaming an address is a state reset** — the node is a remove + add, as in live-coding
  environments.
- **Changing a node's operator type at the same address is a state reset** — its state layout
  is different anyway.
- Everything else — rewired inputs, changed params, new neighbors — leaves a survivor a
  survivor.

**Considered and rejected:** *address only* (a type change at the same address has no
meaningful state to carry; "attempt migration, fall back cold" is murkier semantics for zero
benefit); *an explicit stable `id` field* (survives renames, but adds a second identity the
model must invent and preserve across every re-emission — the authoring noise ADR-0036 §5
refused when it rejected reference pinning).

### 3. Incremental vocabulary: a later rung, seam named now

An incremental edit vocabulary (add-node / rewire / retune) is **not ruled out — it is a later
rung** with a named seam: the **Coordinator command queue** (ADR-0012), where the
whole-document install already lives as a command (≈ `SwapDoc(NormalizedDoc)`). Future
incremental commands slot in as sibling command variants — no architectural change.

One constraint travels with the seam: any future incremental command must resolve to
**apply-to-document → re-validate the whole document → swap**. The loader stays the single
validation authority (ADR-0020) and the document stays the save source of truth (ADR-0036) —
incremental editing, if it ever comes, is sugar over this contract, not a rival contract.

**Considered and rejected:** *ruling it out permanently* (costs a revisit-ADR later for no
present gain); *leaving the seam unnamed* (a future effort would rediscover the command queue
anyway, but might not rediscover the sugar constraint that keeps validation single-authority).

### 4. Scale: nesting is the pressure valve, not a contract change

Token cost of re-emitting the document every turn does not bend the contract:

- Re-emitting a document **never re-emits its subpatch references** (ADR-0036 §1) — a
  well-factored large instrument is several small documents, and one edit touches one of them.
- Every real instrument today is ≤20KB. The documented remedy for a document that outgrows
  comfortable re-emission is **factor it into subpatches** — better authoring anyway — not a
  different edit contract.

Whether the swap/validate tools accept the document **by value** (JSON in the tool call) or
also **by path** (the sidecar loads via `FsResolver` — near-zero tokens for the
dev-with-checkout persona) is MCP/E's tool-contract decision; neither branch strains the
whole-document contract.

### 5. `send` is ephemeral audition; the document is durable truth

Both ADR-0012 paths stay legal in conversation, with defined durability semantics:

- **`send`** (control path, no Swap) is for **audition**: sweeping a cutoff, trying a tempo.
  Its effects live in render state only — the next Swap re-reads inputs from the installed
  document, so un-folded tweaks are **clobbered by design**.
- **Document edit + swap** is for **keeping**: when a tweak is accepted, the agent folds the
  value into the document and swaps.

This gives the conversational loop a natural **try-then-commit** shape, which the authoring
skills and MCP tool descriptions document explicitly: `send` to explore, doc-edit + swap to
keep.

**Considered and rejected:** *Swap-only* (one path, but a tight audition loop becomes a full
Instantiate+Swap per nudge, abandoning the already-built OSC control plane); *`send` survives
Swap* (render state wins over document values for survivors — the document and the sound drift
apart, and the save source of truth quietly stops being true).

## Consequences

- MCP/D inherits a fixed contract: the Coordinator's structure channel carries whole-document
  installs; survivor matching is address+type over built graphs; the command queue is the named
  seam for any future incremental vocabulary.
- MCP/E designs tools against document-in/report-out: validate and swap take a whole document
  (by value or by path — its call); no add-node/rewire tool surface exists or is reserved.
- ADR-0009's "matched by stable identity" is now concrete: no format change, no new id field —
  identity is the address the author already writes.
- Authoring guidance (patcher skill, MCP tool descriptions) documents two rules of thumb:
  rename = state reset, and `send` tweaks are lost at Swap unless folded into the document.
- Very large instruments are an authoring-shape problem (factor into subpatches), not an edit
  contract problem.
